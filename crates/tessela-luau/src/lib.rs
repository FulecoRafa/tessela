//! Luau engine (mlua).
//!
//! Luau, not PUC Lua: it was built by Roblox to run untrusted scripts, so the
//! sandbox is a first-class mode rather than a list of globals to delete.
//! `Lua::sandbox(true)` makes every built-in library and every prelude
//! read-only and gives the script its own global table.
//!
//! Load order: core prelude (signals, `log`, components) → module preludes
//! (each sees the private `__rt` helpers) → `__rt`/`__host` removed → sandbox
//! sealed → extension code.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use mlua::{Function, Lua, Table, Value as LuaValue, VmState};
use serde_json::{Map, Number, Value};
use tessela_core::{Engine, EngineFactory, FnId, GuestError, HostCtx, Lang, fn_kind, marker, view};

const PRELUDE: &str = include_str!("prelude.luau");

/// Factory for `.luau` / `.lua` extensions.
pub struct Luau;

impl EngineFactory for Luau {
    fn name(&self) -> &str {
        "luau"
    }
    fn extensions(&self) -> &[&str] {
        &["luau", "lua"]
    }
    fn create(&self, host: Rc<HostCtx>) -> Result<Box<dyn Engine>, GuestError> {
        Ok(Box::new(LuaEngine::new(host)?))
    }
}

type Fns = Rc<RefCell<HashMap<FnId, Function>>>;

struct LuaEngine {
    lua: Lua,
    host: Rc<HostCtx>,
    fns: Fns,
}

fn guest_err(e: mlua::Error) -> GuestError {
    // Keep the message, drop the Rust-side traceback noise.
    let msg = e.to_string();
    GuestError(
        msg.lines()
            .next()
            .unwrap_or("lua error")
            .trim_start_matches("runtime error: ")
            .to_string(),
    )
}

fn host_err(e: impl std::fmt::Display) -> mlua::Error {
    mlua::Error::runtime(e.to_string())
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Plain data (signal values): functions are an error.
    Reject,
    /// View trees: functions become bindings or handlers by prop name.
    View,
    /// Module arguments: functions become handlers.
    Module,
}

/// Converts guest values to JSON, registering any functions it meets.
struct ToJson<'a> {
    host: &'a HostCtx,
    fns: &'a Fns,
    budget: usize,
    mode: Mode,
}

impl ToJson<'_> {
    fn new<'a>(host: &'a HostCtx, fns: &'a Fns, mode: Mode) -> ToJson<'a> {
        ToJson {
            host,
            fns,
            budget: host.limits.max_values,
            mode,
        }
    }

    fn conv(&mut self, v: LuaValue, key: Option<&str>, depth: usize) -> mlua::Result<Value> {
        if depth > self.host.limits.max_depth {
            return Err(host_err("value nested too deeply"));
        }
        if self.budget == 0 {
            return Err(host_err("value too large"));
        }
        self.budget -= 1;
        Ok(match v {
            LuaValue::Nil => Value::Null,
            LuaValue::Boolean(b) => Value::Bool(b),
            LuaValue::Integer(i) => Value::Number(i.into()),
            LuaValue::Number(n) => Number::from_f64(n)
                .map(Value::Number)
                .unwrap_or(Value::Null),
            LuaValue::String(s) => Value::String(s.to_string_lossy()),
            LuaValue::Table(t) => self.table(t, depth)?,
            LuaValue::Function(f) => {
                if self.mode == Mode::Reject {
                    return Err(host_err("functions cannot be stored in signals"));
                }
                let kind = fn_kind(key, self.mode == Mode::Module);
                let id = self.host.core.borrow_mut().register_fn(kind);
                self.fns.borrow_mut().insert(id, f);
                marker(kind, id)
            }
            other => return Err(host_err(format!("cannot use a {} here", other.type_name()))),
        })
    }

    fn table(&mut self, t: Table, depth: usize) -> mlua::Result<Value> {
        let len = t.raw_len();
        let mut count = 0usize;
        for pair in t.clone().pairs::<LuaValue, LuaValue>() {
            pair?;
            count += 1;
        }
        if len > 0 && count == len {
            let mut out = Vec::with_capacity(len);
            for i in 1..=len {
                out.push(self.conv(t.raw_get(i)?, None, depth + 1)?);
            }
            return Ok(Value::Array(out));
        }
        let mut out = Map::new();
        for pair in t.pairs::<LuaValue, LuaValue>() {
            let (k, v) = pair?;
            let key = match k {
                LuaValue::String(s) => s.to_string_lossy(),
                LuaValue::Integer(i) => i.to_string(),
                other => {
                    return Err(host_err(format!(
                        "table keys must be strings, got {}",
                        other.type_name()
                    )));
                }
            };
            let val = self.conv(v, Some(&key), depth + 1)?;
            out.insert(key, val);
        }
        Ok(Value::Object(out))
    }
}

fn to_lua(lua: &Lua, v: &Value) -> mlua::Result<LuaValue> {
    Ok(match v {
        Value::Null => LuaValue::Nil,
        Value::Bool(b) => LuaValue::Boolean(*b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => LuaValue::Integer(i),
            None => LuaValue::Number(n.as_f64().unwrap_or(0.0)),
        },
        Value::String(s) => LuaValue::String(lua.create_string(s)?),
        Value::Array(items) => {
            let t = lua.create_table_with_capacity(items.len(), 0)?;
            for (i, item) in items.iter().enumerate() {
                t.raw_set(i + 1, to_lua(lua, item)?)?;
            }
            LuaValue::Table(t)
        }
        Value::Object(m) => {
            let t = lua.create_table_with_capacity(0, m.len())?;
            for (k, val) in m {
                t.raw_set(k.as_str(), to_lua(lua, val)?)?;
            }
            LuaValue::Table(t)
        }
    })
}

impl LuaEngine {
    fn new(host: Rc<HostCtx>) -> Result<Self, GuestError> {
        let lua = Lua::new();
        lua.set_memory_limit(host.limits.memory_bytes)
            .map_err(guest_err)?;

        let h = host.clone();
        lua.set_interrupt(move |_| {
            if h.budget_exceeded() {
                Err(mlua::Error::runtime("CPU budget exceeded"))
            } else {
                Ok(VmState::Continue)
            }
        });

        let fns: Fns = Rc::default();
        let g = lua.globals();
        g.set(
            "__host",
            Self::host_table(&lua, &host, &fns).map_err(guest_err)?,
        )
        .map_err(guest_err)?;
        // Nothing that loads code or reaches the environment of other chunks.
        for name in [
            "loadstring",
            "require",
            "getfenv",
            "setfenv",
            "collectgarbage",
            "newproxy",
        ] {
            g.set(name, LuaValue::Nil).map_err(guest_err)?;
        }
        lua.load(PRELUDE)
            .set_name("=prelude")
            .exec()
            .map_err(guest_err)?;
        for (name, code) in host.module_preludes(Lang::Luau) {
            lua.load(code)
                .set_name(format!("=module:{name}"))
                .exec()
                .map_err(guest_err)?;
        }
        g.set("__rt", LuaValue::Nil).map_err(guest_err)?;
        g.set("__host", LuaValue::Nil).map_err(guest_err)?;
        // From here on, built-ins and preludes are read-only and every script
        // gets a fresh global table.
        lua.sandbox(true).map_err(guest_err)?;
        Ok(Self { lua, host, fns })
    }

    fn host_table(lua: &Lua, host: &Rc<HostCtx>, fns: &Fns) -> mlua::Result<Table> {
        let t = lua.create_table()?;

        let (h, f) = (host.clone(), fns.clone());
        t.set(
            "new",
            lua.create_function(move |_, v: LuaValue| {
                let v = ToJson::new(&h, &f, Mode::Reject).conv(v, None, 0)?;
                Ok(h.core.borrow_mut().sig_new(v))
            })?,
        )?;
        let h = host.clone();
        t.set(
            "get",
            lua.create_function(move |lua, id: u64| {
                let v = h.core.borrow_mut().sig_get(id).map_err(host_err)?;
                to_lua(lua, &v)
            })?,
        )?;
        let h = host.clone();
        t.set(
            "peek",
            lua.create_function(move |lua, id: u64| {
                let v = h.core.borrow().sig_peek(id).map_err(host_err)?;
                to_lua(lua, &v)
            })?,
        )?;
        let (h, f) = (host.clone(), fns.clone());
        t.set(
            "set",
            lua.create_function(move |_, (id, v): (u64, LuaValue)| {
                let v = ToJson::new(&h, &f, Mode::Reject).conv(v, None, 0)?;
                h.core.borrow_mut().sig_set(id, v).map_err(host_err)
            })?,
        )?;
        let (h, f) = (host.clone(), fns.clone());
        t.set(
            "invoke",
            lua.create_function(
                move |lua, (module, func, args): (String, String, LuaValue)| {
                    let args = ToJson::new(&h, &f, Mode::Module).conv(args, None, 0)?;
                    let ret = h.invoke(&module, &func, args).map_err(host_err)?;
                    to_lua(lua, &ret)
                },
            )?,
        )?;
        let h = host.clone();
        t.set(
            "log",
            lua.create_function(move |_, msg: String| {
                h.core.borrow_mut().log(msg);
                Ok(())
            })?,
        )?;
        let components = lua.create_table()?;
        for (i, c) in view::COMPONENTS.iter().enumerate() {
            let e = lua.create_table()?;
            e.set("name", c.name)?;
            e.set("shorthand", c.shorthand)?;
            components.raw_set(i + 1, e)?;
        }
        t.set("components", components)?;
        Ok(t)
    }

    fn convert(&self, v: LuaValue) -> mlua::Result<Value> {
        ToJson::new(&self.host, &self.fns, Mode::View).conv(v, None, 0)
    }
}

impl Engine for LuaEngine {
    fn load(&mut self, source: &str, file: &str) -> Result<Value, GuestError> {
        let ret: LuaValue = self
            .lua
            .load(source)
            .set_name(format!("@{file}"))
            .eval()
            .map_err(guest_err)?;
        match ret {
            // `return function() ... end`: the whole widget is one binding and
            // re-renders when anything it read changes (React-style).
            LuaValue::Function(_) | LuaValue::Table(_) => self.convert(ret).map_err(guest_err),
            other => Err(GuestError(format!(
                "extension must return a component or a function, got {}",
                other.type_name()
            ))),
        }
    }

    fn call(&mut self, f: FnId, args: Value) -> Result<Value, GuestError> {
        let func = self
            .fns
            .borrow()
            .get(&f)
            .cloned()
            .ok_or_else(|| GuestError(format!("unknown function {f}")))?;
        let arg = to_lua(&self.lua, &args).map_err(guest_err)?;
        let ret: LuaValue = func.call(arg).map_err(guest_err)?;
        self.convert(ret).map_err(guest_err)
    }

    fn drop_fns(&mut self, ids: &[FnId]) {
        let mut fns = self.fns.borrow_mut();
        for id in ids {
            fns.remove(id);
        }
    }
}
