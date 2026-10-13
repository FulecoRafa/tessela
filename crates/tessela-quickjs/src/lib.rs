//! JavaScript/TSX engine (QuickJS-NG via rquickjs, TSX via oxc).
//!
//! The source is transpiled on the host (types stripped, JSX turned into
//! `h(...)` calls) and evaluated as an ES module. Without [`Sources`] there is
//! **no module loader**: any `import` fails, so the extension sees only the
//! frozen globals defined by the preludes. With them, relative imports of the
//! extension's own files work — resolved inside its root by the host, never
//! reaching the filesystem from here — and nothing else does. QuickJS has no
//! `fetch`, `fs` or timers unless the embedder adds them; everything of that
//! kind comes from modules.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use rquickjs::loader::{ImportAttributes, Loader, Resolver};
use rquickjs::module::Declared;
use rquickjs::{Context, Ctx, Function, Module, Persistent, Runtime, Type, Value as JsValue};
use serde_json::{Map, Number, Value};
use tessela_core::{
    Engine, EngineFactory, FnId, GuestError, HostCtx, IMPORT_EXTENSIONS, Lang, Sources, fn_kind,
    marker, prop_key, resolve_import,
};

const PRELUDE: &str = include_str!("prelude.js");

/// Factory for `.tsx`, `.ts`, `.jsx` and `.js` extensions.
pub struct QuickJs;

impl EngineFactory for QuickJs {
    fn name(&self) -> &str {
        "quickjs"
    }
    fn extensions(&self) -> &[&str] {
        &["tsx", "ts", "jsx", "js"]
    }
    fn create(&self, host: Rc<HostCtx>) -> Result<Box<dyn Engine>, GuestError> {
        Ok(Box::new(JsEngine::new(host)?))
    }
}

type FnMap = Rc<RefCell<HashMap<FnId, Persistent<Function<'static>>>>>;

struct JsEngine {
    // Field order is drop order: persistent handles must go before the
    // context, and the context before the runtime, or QuickJS aborts.
    fns: FnMap,
    host: Rc<HostCtx>,
    ctx: Context,
    _rt: Runtime,
}

impl Drop for JsEngine {
    fn drop(&mut self) {
        // Host closures inside the context also hold `fns`; empty the map so
        // no Persistent outlives the context either way.
        self.fns.borrow_mut().clear();
    }
}

/// TSX/TS/JSX → plain ES module.
pub fn transpile(source: &str, file: &str) -> Result<String, GuestError> {
    use oxc::allocator::Allocator;
    use oxc::codegen::Codegen;
    use oxc::parser::Parser;
    use oxc::semantic::SemanticBuilder;
    use oxc::span::SourceType;
    use oxc::transformer::{JsxOptions, JsxRuntime, TransformOptions, Transformer};

    let path = Path::new(file);
    let source_type =
        SourceType::from_path(path).map_err(|e| GuestError(format!("{file}: {e}")))?;
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if let Some(err) = parsed.diagnostics.first() {
        return Err(GuestError(format!("{file}: {err}")));
    }
    let mut program = parsed.program;
    let scoping = SemanticBuilder::new()
        .build(&program)
        .semantic
        .into_scoping();
    let options = TransformOptions {
        jsx: JsxOptions {
            runtime: JsxRuntime::Classic,
            pragma: Some("h".into()),
            pragma_frag: Some("Fragment".into()),
            pure: false,
            display_name_plugin: false,
            ..JsxOptions::enable()
        },
        ..TransformOptions::default()
    };
    let ret =
        Transformer::new(&allocator, path, &options).build_with_scoping(scoping, &mut program);
    if let Some(err) = ret.diagnostics.first() {
        return Err(GuestError(format!("{file}: {err}")));
    }
    Ok(Codegen::new().build(&program).code)
}

fn exception(ctx: &Ctx<'_>, e: rquickjs::Error) -> GuestError {
    if !matches!(e, rquickjs::Error::Exception) {
        return GuestError(e.to_string());
    }
    let caught = ctx.catch();
    if let Some(ex) = caught.as_exception() {
        let msg = ex.message().unwrap_or_else(|| "exception".into());
        // QuickJS reports a tripped interrupt as a plain "interrupted" error.
        if msg == "interrupted" {
            return GuestError("CPU budget exceeded".into());
        }
        return GuestError(msg);
    }
    if caught.is_null() {
        // QuickJS throws `null` when it cannot even allocate the error object.
        return GuestError("out of memory".into());
    }
    GuestError(
        caught
            .as_string()
            .and_then(|s| s.to_string().ok())
            .unwrap_or_else(|| format!("uncaught {}", caught.type_name())),
    )
}

fn throw(ctx: &Ctx<'_>, msg: impl std::fmt::Display) -> rquickjs::Error {
    rquickjs::Exception::throw_message(ctx, &msg.to_string())
}

/// Resolves and loads `import`s through the host's [`Sources`]. Files found
/// while resolving are kept until loaded, so each one is read once.
struct ProjectLoader {
    host: Rc<HostCtx>,
    sources: std::sync::Arc<dyn Sources>,
    found: Rc<RefCell<HashMap<String, String>>>,
}

impl ProjectLoader {
    fn find(&self, target: &str) -> Option<(String, String)> {
        let has_ext = Path::new(target)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| JS_EXTENSIONS.contains(&e));
        let mut candidates = Vec::new();
        if has_ext {
            candidates.push(target.to_string());
        }
        for ext in IMPORT_EXTENSIONS
            .iter()
            .filter(|e| JS_EXTENSIONS.contains(e))
        {
            candidates.push(format!("{target}.{ext}"));
        }
        for ext in IMPORT_EXTENSIONS
            .iter()
            .filter(|e| JS_EXTENSIONS.contains(e))
        {
            candidates.push(format!("{target}/index.{ext}"));
        }
        candidates
            .into_iter()
            .find_map(|c| self.sources.read(&c).map(|s| (c, s)))
    }
}

const JS_EXTENSIONS: &[&str] = &["tsx", "ts", "jsx", "js"];

impl Resolver for ProjectLoader {
    fn resolve<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        base: &str,
        name: &str,
        _attributes: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<String> {
        let target = resolve_import(base, name).map_err(|e| throw(ctx, e))?;
        let (path, source) = self
            .find(&target)
            .ok_or_else(|| throw(ctx, format!("cannot find '{name}' (imported from {base})")))?;
        self.found.borrow_mut().insert(path.clone(), source);
        Ok(path)
    }
}

impl Loader for ProjectLoader {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        name: &str,
        _attributes: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<Module<'js, Declared>> {
        let source = match self.found.borrow_mut().remove(name) {
            Some(s) => s,
            None => self
                .sources
                .read(name)
                .ok_or_else(|| throw(ctx, format!("cannot load '{name}'")))?,
        };
        let code = transpile(&source, name).map_err(|e| throw(ctx, e))?;
        let module = Module::declare(ctx.clone(), name, code)?;
        // Reading and transpiling ran on the host; it is not guest CPU time.
        self.host.rearm();
        Ok(module)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// View trees: functions become bindings or handlers by prop name.
    View,
    /// Module arguments: functions become handlers.
    Module,
}

struct ToJson<'a> {
    host: &'a HostCtx,
    fns: &'a FnMap,
    budget: usize,
    mode: Mode,
}

impl ToJson<'_> {
    fn new<'a>(host: &'a HostCtx, fns: &'a FnMap, mode: Mode) -> ToJson<'a> {
        ToJson {
            host,
            fns,
            budget: host.limits.max_values,
            mode,
        }
    }

    fn conv<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        v: JsValue<'js>,
        key: Option<&str>,
        depth: usize,
    ) -> Result<Value, GuestError> {
        if depth > self.host.limits.max_depth {
            return Err(GuestError("value nested too deeply".into()));
        }
        if self.budget == 0 {
            return Err(GuestError("value too large".into()));
        }
        self.budget -= 1;
        Ok(match v.type_of() {
            Type::Uninitialized | Type::Undefined | Type::Null => Value::Null,
            Type::Bool => Value::Bool(v.as_bool().unwrap_or(false)),
            Type::Int => Value::Number(v.as_int().unwrap_or(0).into()),
            Type::Float => Number::from_f64(v.as_float().unwrap_or(0.0))
                .map(Value::Number)
                .unwrap_or(Value::Null),
            Type::String => Value::String(
                v.as_string()
                    .and_then(|s| s.to_string().ok())
                    .unwrap_or_default(),
            ),
            Type::Array => {
                let arr = v.as_array().expect("type checked");
                let mut out = Vec::with_capacity(arr.len());
                for item in arr.iter::<JsValue>() {
                    let item = item.map_err(|e| exception(ctx, e))?;
                    out.push(self.conv(ctx, item, None, depth + 1)?);
                }
                Value::Array(out)
            }
            Type::Function | Type::Constructor => {
                let f = v.into_function().expect("type checked");
                let kind = fn_kind(key, self.mode == Mode::Module);
                let id = self.host.core.borrow_mut().register_fn(kind);
                self.fns.borrow_mut().insert(id, Persistent::save(ctx, f));
                marker(kind, id)
            }
            Type::Object | Type::Exception => {
                let obj = v.into_object().expect("type checked");
                let mut out = Map::new();
                for prop in obj.props::<String, JsValue>() {
                    let (k, val) = prop.map_err(|e| exception(ctx, e))?;
                    let k = prop_key(&k);
                    let val = self.conv(ctx, val, Some(&k), depth + 1)?;
                    out.insert(k, val);
                }
                Value::Object(out)
            }
            other => return Err(GuestError(format!("cannot use a {other} here"))),
        })
    }
}

fn install_host<'js>(ctx: &Ctx<'js>, host: &Rc<HostCtx>, fns: &FnMap) -> rquickjs::Result<()> {
    let t = rquickjs::Object::new(ctx.clone())?;
    let parse = |s: &str| serde_json::from_str::<Value>(s).unwrap_or(Value::Null);

    let h = host.clone();
    t.set(
        "sigNew",
        Function::new(ctx.clone(), move |json: String| -> u64 {
            h.core.borrow_mut().sig_new(parse(&json))
        })?,
    )?;
    let h = host.clone();
    t.set(
        "get",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, id: u64| -> rquickjs::Result<String> {
                Ok(h.core
                    .borrow_mut()
                    .sig_get(id)
                    .map_err(|e| throw(&ctx, e))?
                    .to_string())
            },
        )?,
    )?;
    let h = host.clone();
    t.set(
        "peek",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, id: u64| -> rquickjs::Result<String> {
                Ok(h.core
                    .borrow()
                    .sig_peek(id)
                    .map_err(|e| throw(&ctx, e))?
                    .to_string())
            },
        )?,
    )?;
    let h = host.clone();
    t.set(
        "set",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, id: u64, json: String| -> rquickjs::Result<()> {
                h.core
                    .borrow_mut()
                    .sig_set(id, parse(&json))
                    .map_err(|e| throw(&ctx, e))
            },
        )?,
    )?;
    let (h, f) = (host.clone(), fns.clone());
    t.set(
        "invoke",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>,
                  module: String,
                  func: String,
                  args: JsValue<'js>|
                  -> rquickjs::Result<String> {
                let args = ToJson::new(&h, &f, Mode::Module)
                    .conv(&ctx, args, None, 0)
                    .map_err(|e| throw(&ctx, e))?;
                let ret = h.invoke(&module, &func, args).map_err(|e| throw(&ctx, e))?;
                Ok(ret.to_string())
            },
        )?,
    )?;
    let h = host.clone();
    t.set(
        "persist",
        Function::new(ctx.clone(), move |key: String, json: String| -> u64 {
            h.core.borrow_mut().sig_persist(&key, parse(&json))
        })?,
    )?;
    let h = host.clone();
    t.set(
        "log",
        Function::new(ctx.clone(), move |msg: String| {
            h.core.borrow_mut().log(msg);
        })?,
    )?;
    let components: Vec<Value> = host
        .components()
        .map(|(name, shorthand)| serde_json::json!({ "name": name, "shorthand": shorthand }))
        .collect();
    t.set("components", Value::Array(components).to_string())?;
    ctx.globals().set("__host", t)?;
    Ok(())
}

impl JsEngine {
    fn new(host: Rc<HostCtx>) -> Result<Self, GuestError> {
        let rt = Runtime::new().map_err(|e| GuestError(e.to_string()))?;
        rt.set_memory_limit(host.limits.memory_bytes);
        rt.set_max_stack_size(512 * 1024);
        let h = host.clone();
        rt.set_interrupt_handler(Some(Box::new(move || h.budget_exceeded())));
        if let Some(sources) = host.sources() {
            let loader = ProjectLoader {
                host: host.clone(),
                sources: sources.clone(),
                found: Rc::default(),
            };
            let resolver = ProjectLoader {
                host: host.clone(),
                sources: sources.clone(),
                found: loader.found.clone(),
            };
            rt.set_loader(resolver, loader);
        }
        let ctx = Context::full(&rt).map_err(|e| GuestError(e.to_string()))?;
        let fns: FnMap = Rc::default();
        ctx.with(|ctx| -> Result<(), GuestError> {
            install_host(&ctx, &host, &fns).map_err(|e| exception(&ctx, e))?;
            ctx.eval::<(), _>(PRELUDE).map_err(|e| exception(&ctx, e))?;
            for (name, code) in host.module_preludes(Lang::JavaScript) {
                ctx.eval::<(), _>(code).map_err(|e| {
                    GuestError(format!("module {name} prelude: {}", exception(&ctx, e)))
                })?;
            }
            // Seal: the private helpers disappear before extension code runs.
            ctx.eval::<(), _>("delete globalThis.__rt; delete globalThis.__host;")
                .map_err(|e| exception(&ctx, e))
        })?;
        Ok(Self {
            fns,
            host,
            ctx,
            _rt: rt,
        })
    }
}

impl Engine for JsEngine {
    fn load(&mut self, source: &str, file: &str) -> Result<Value, GuestError> {
        let code = transpile(source, file)?;
        let (host, fns) = (self.host.clone(), self.fns.clone());
        self.ctx.with(|ctx| {
            let declared =
                Module::declare(ctx.clone(), file, code).map_err(|e| exception(&ctx, e))?;
            let (module, promise) = declared.eval().map_err(|e| exception(&ctx, e))?;
            promise.finish::<()>().map_err(|e| exception(&ctx, e))?;
            let default: JsValue = module.get("default").map_err(|e| exception(&ctx, e))?;
            // A function default export is a component: call it once, Solid-style.
            let tree = match default.as_function() {
                Some(f) => {
                    let props =
                        rquickjs::Object::new(ctx.clone()).map_err(|e| exception(&ctx, e))?;
                    f.call::<_, JsValue>((props,))
                        .map_err(|e| exception(&ctx, e))?
                }
                None if default.is_object() => default,
                None => {
                    return Err(GuestError(
                        "extension must `export default` a component or a node".into(),
                    ));
                }
            };
            ToJson::new(&host, &fns, Mode::View).conv(&ctx, tree, None, 0)
        })
    }

    fn call(&mut self, f: FnId, args: Value) -> Result<Value, GuestError> {
        let persistent = self
            .fns
            .borrow()
            .get(&f)
            .cloned()
            .ok_or_else(|| GuestError(format!("unknown function {f}")))?;
        let (host, fns) = (self.host.clone(), self.fns.clone());
        self.ctx.with(|ctx| {
            let func = persistent.restore(&ctx).map_err(|e| exception(&ctx, e))?;
            let arg: JsValue = ctx
                .json_parse(args.to_string())
                .map_err(|e| exception(&ctx, e))?;
            let ret: JsValue = func.call((arg,)).map_err(|e| exception(&ctx, e))?;
            ToJson::new(&host, &fns, Mode::View).conv(&ctx, ret, None, 0)
        })
    }

    fn drop_fns(&mut self, ids: &[FnId]) {
        let mut fns = self.fns.borrow_mut();
        for id in ids {
            fns.remove(id);
        }
    }
}
