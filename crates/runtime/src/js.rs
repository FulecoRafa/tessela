//! TSX/JS adapter (QuickJS-NG via rquickjs, TSX via oxc).
//!
//! The widget source is transpiled on the host (types stripped, JSX turned
//! into `h(...)` calls) and evaluated as an ES module with **no module
//! loader**: any `import` of a value fails, so the widget sees only the
//! frozen globals from the prelude. QuickJS has no `fetch`, `fs` or timers
//! unless the embedder adds them, and we add none.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::time::Instant;

use rquickjs::function::Opt;
use rquickjs::{Context, Ctx, Function, Module, Persistent, Runtime, Type, Value as JsValue};
use serde_json::{Map, Number, Value};

use crate::core::FnKind;
use crate::{Engine, FnId, GuestError, Shared, is_handler_key, prop_key};

const PRELUDE: &str = include_str!("prelude.js");

type FnMap = Rc<RefCell<HashMap<FnId, Persistent<Function<'static>>>>>;

pub(crate) struct JsEngine {
    // Field order is drop order: persistent handles must go before the
    // context, and the context before the runtime, or QuickJS aborts.
    fns: FnMap,
    shared: Rc<Shared>,
    ctx: Context,
    _rt: Runtime,
}

impl Drop for JsEngine {
    fn drop(&mut self) {
        // The host `call` closure inside the context also holds `fns`; empty
        // the map so no Persistent outlives the context either way.
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

struct ToJson<'a> {
    shared: &'a Shared,
    fns: &'a FnMap,
    budget: usize,
}

impl ToJson<'_> {
    fn conv<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        v: JsValue<'js>,
        key: Option<&str>,
        depth: usize,
    ) -> Result<Value, GuestError> {
        if depth > 64 {
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
                let kind = if key.is_some_and(is_handler_key) {
                    FnKind::Handler
                } else {
                    FnKind::Binding
                };
                let id = self.shared.core.borrow_mut().register_fn(kind);
                self.fns.borrow_mut().insert(id, Persistent::save(ctx, f));
                let marker = if kind == FnKind::Handler {
                    "$fn"
                } else {
                    "$bind"
                };
                serde_json::json!({ marker: id })
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

fn install_host<'js>(ctx: &Ctx<'js>, shared: &Rc<Shared>, fns: &FnMap) -> rquickjs::Result<()> {
    let host = rquickjs::Object::new(ctx.clone())?;
    let parse = |s: &str| serde_json::from_str::<Value>(s).unwrap_or(Value::Null);

    let sh = shared.clone();
    host.set(
        "sigNew",
        Function::new(ctx.clone(), move |json: String| -> u64 {
            sh.core.borrow_mut().sig_new(parse(&json))
        })?,
    )?;
    let sh = shared.clone();
    host.set(
        "get",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, id: u64| -> rquickjs::Result<String> {
                let v = sh
                    .core
                    .borrow_mut()
                    .sig_get(id)
                    .map_err(|e| throw(&ctx, e))?;
                Ok(v.to_string())
            },
        )?,
    )?;
    let sh = shared.clone();
    host.set(
        "peek",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, id: u64| -> rquickjs::Result<String> {
                let v = sh.core.borrow().sig_peek(id).map_err(|e| throw(&ctx, e))?;
                Ok(v.to_string())
            },
        )?,
    )?;
    let sh = shared.clone();
    host.set(
        "set",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, id: u64, json: String| -> rquickjs::Result<()> {
                sh.core
                    .borrow_mut()
                    .sig_set(id, parse(&json))
                    .map_err(|e| throw(&ctx, e))
            },
        )?,
    )?;
    let sh = shared.clone();
    host.set(
        "use",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, name: String| -> rquickjs::Result<()> {
                sh.core
                    .borrow()
                    .check_use(&name)
                    .map_err(|e| throw(&ctx, e))
            },
        )?,
    )?;
    let sh = shared.clone();
    host.set(
        "subscribe",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, service: String, path: String| -> rquickjs::Result<u64> {
                sh.core
                    .borrow_mut()
                    .subscribe(&service, &path)
                    .map_err(|e| throw(&ctx, e))
            },
        )?,
    )?;
    let sh = shared.clone();
    let f = fns.clone();
    host.set(
        "call",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>,
                  service: String,
                  method: String,
                  args: String,
                  cb: Opt<Option<Function<'js>>>|
                  -> rquickjs::Result<()> {
                let cb_id = cb.0.flatten().map(|cb| {
                    let id = sh.core.borrow_mut().register_fn(FnKind::Handler);
                    f.borrow_mut().insert(id, Persistent::save(&ctx, cb));
                    id
                });
                sh.core
                    .borrow_mut()
                    .call(&service, &method, parse(&args), cb_id)
                    .map_err(|e| throw(&ctx, e))?;
                Ok(())
            },
        )?,
    )?;
    let sh = shared.clone();
    host.set(
        "log",
        Function::new(ctx.clone(), move |msg: String| {
            sh.core.borrow_mut().log(msg);
        })?,
    )?;
    ctx.globals().set("__host", host)?;
    Ok(())
}

impl JsEngine {
    pub(crate) fn new(shared: Rc<Shared>) -> Result<Self, GuestError> {
        let rt = Runtime::new().map_err(|e| GuestError(e.to_string()))?;
        rt.set_memory_limit(shared.limits.memory_bytes);
        rt.set_max_stack_size(512 * 1024);
        let sh = shared.clone();
        rt.set_interrupt_handler(Some(Box::new(move || Instant::now() > sh.deadline.get())));
        let ctx = Context::full(&rt).map_err(|e| GuestError(e.to_string()))?;
        let fns: FnMap = Rc::default();
        ctx.with(|ctx| -> Result<(), GuestError> {
            install_host(&ctx, &shared, &fns).map_err(|e| exception(&ctx, e))?;
            ctx.eval::<(), _>(PRELUDE).map_err(|e| exception(&ctx, e))
        })?;
        Ok(Self {
            fns,
            shared,
            ctx,
            _rt: rt,
        })
    }
}

impl Engine for JsEngine {
    fn load(&mut self, source: &str, file: &str) -> Result<Value, GuestError> {
        let code = transpile(source, file)?;
        let (shared, fns) = (self.shared.clone(), self.fns.clone());
        self.ctx.with(|ctx| {
            let declared =
                Module::declare(ctx.clone(), file, code).map_err(|e| exception(&ctx, e))?;
            let (module, promise) = declared.eval().map_err(|e| exception(&ctx, e))?;
            promise.finish::<()>().map_err(|e| exception(&ctx, e))?;
            let default: JsValue = module.get("default").map_err(|e| exception(&ctx, e))?;
            // A function default export is a component: call it once, Solid-style.
            let tree = match default.as_function() {
                Some(f) => f
                    .call::<_, JsValue>((
                        rquickjs::Object::new(ctx.clone()).map_err(|e| exception(&ctx, e))?,
                    ))
                    .map_err(|e| exception(&ctx, e))?,
                None if default.is_object() => default,
                None => {
                    return Err(GuestError(
                        "widget must `export default` a component or a node".into(),
                    ));
                }
            };
            ToJson {
                shared: &shared,
                fns: &fns,
                budget: shared.limits.max_values,
            }
            .conv(&ctx, tree, None, 0)
        })
    }

    fn call(&mut self, f: FnId, args: Value) -> Result<Value, GuestError> {
        let persistent = self
            .fns
            .borrow()
            .get(&f)
            .cloned()
            .ok_or_else(|| GuestError(format!("unknown function {f}")))?;
        let (shared, fns) = (self.shared.clone(), self.fns.clone());
        self.ctx.with(|ctx| {
            let func = persistent.restore(&ctx).map_err(|e| exception(&ctx, e))?;
            let arg: JsValue = ctx
                .json_parse(args.to_string())
                .map_err(|e| exception(&ctx, e))?;
            let ret: JsValue = func.call((arg,)).map_err(|e| exception(&ctx, e))?;
            ToJson {
                shared: &shared,
                fns: &fns,
                budget: shared.limits.max_values,
            }
            .conv(&ctx, ret, None, 0)
        })
    }

    fn drop_fns(&mut self, ids: &[FnId]) {
        let mut fns = self.fns.borrow_mut();
        for id in ids {
            fns.remove(id);
        }
    }
}
