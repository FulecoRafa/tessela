//! Luau adapter (mlua).
//!
//! Luau, not PUC Lua: it was built by Roblox to run untrusted scripts, so the
//! sandbox is a first-class mode rather than a list of globals to delete.
//! `Lua::sandbox(true)` makes every built-in library and our prelude
//! read-only and gives the script its own global table.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use mlua::{Function, Lua, Table, Value as LuaValue, VmState};
use serde_json::{Map, Number, Value};

use crate::core::FnKind;
use crate::{Engine, FnId, GuestError, Shared, is_handler_key};

const PRELUDE: &str = include_str!("prelude.luau");

pub(crate) struct LuaEngine {
    lua: Lua,
    shared: Rc<Shared>,
    fns: Rc<RefCell<HashMap<FnId, Function>>>,
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

/// Converts guest values to JSON, registering any functions it meets.
struct ToJson<'a> {
    shared: &'a Shared,
    fns: &'a RefCell<HashMap<FnId, Function>>,
    budget: usize,
    allow_fns: bool,
}

impl ToJson<'_> {
    fn conv(&mut self, v: LuaValue, key: Option<&str>, depth: usize) -> mlua::Result<Value> {
        if depth > 64 {
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
                if !self.allow_fns {
                    return Err(host_err("functions cannot be sent to services"));
                }
                let kind = if key.is_some_and(is_handler_key) {
                    FnKind::Handler
                } else {
                    FnKind::Binding
                };
                let id = self.shared.core.borrow_mut().register_fn(kind);
                self.fns.borrow_mut().insert(id, f);
                let marker = if kind == FnKind::Handler {
                    "$fn"
                } else {
                    "$bind"
                };
                serde_json::json!({ marker: id })
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
    pub(crate) fn new(shared: Rc<Shared>) -> Result<Self, GuestError> {
        let lua = Lua::new();
        lua.set_memory_limit(shared.limits.memory_bytes)
            .map_err(guest_err)?;

        let sh = shared.clone();
        lua.set_interrupt(move |_| {
            if Instant::now() > sh.deadline.get() {
                Err(mlua::Error::runtime("CPU budget exceeded"))
            } else {
                Ok(VmState::Continue)
            }
        });

        let fns: Rc<RefCell<HashMap<FnId, Function>>> = Rc::default();
        let host = Self::host_table(&lua, &shared, &fns).map_err(guest_err)?;
        let g = lua.globals();
        g.set("__host", host).map_err(guest_err)?;
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
        g.set("__host", LuaValue::Nil).map_err(guest_err)?;
        // From here on, built-ins and prelude are read-only and every script
        // gets a fresh global table.
        lua.sandbox(true).map_err(guest_err)?;
        Ok(Self { lua, shared, fns })
    }

    fn host_table(
        lua: &Lua,
        shared: &Rc<Shared>,
        fns: &Rc<RefCell<HashMap<FnId, Function>>>,
    ) -> mlua::Result<Table> {
        let host = lua.create_table()?;

        let sh = shared.clone();
        host.set(
            "new",
            lua.create_function(move |_, v: LuaValue| {
                let v = ToJson {
                    shared: &sh,
                    fns: &RefCell::default(),
                    budget: 10_000,
                    allow_fns: false,
                }
                .conv(v, None, 0)?;
                Ok(sh.core.borrow_mut().sig_new(v))
            })?,
        )?;
        let sh = shared.clone();
        host.set(
            "get",
            lua.create_function(move |lua, id: u64| {
                let v = sh.core.borrow_mut().sig_get(id).map_err(host_err)?;
                to_lua(lua, &v)
            })?,
        )?;
        let sh = shared.clone();
        host.set(
            "peek",
            lua.create_function(move |lua, id: u64| {
                let v = sh.core.borrow().sig_peek(id).map_err(host_err)?;
                to_lua(lua, &v)
            })?,
        )?;
        let sh = shared.clone();
        host.set(
            "set",
            lua.create_function(move |_, (id, v): (u64, LuaValue)| {
                let v = ToJson {
                    shared: &sh,
                    fns: &RefCell::default(),
                    budget: 10_000,
                    allow_fns: false,
                }
                .conv(v, None, 0)?;
                sh.core.borrow_mut().sig_set(id, v).map_err(host_err)
            })?,
        )?;
        let sh = shared.clone();
        host.set(
            "use",
            lua.create_function(move |_, name: String| {
                sh.core.borrow().check_use(&name).map_err(host_err)
            })?,
        )?;
        let sh = shared.clone();
        host.set(
            "subscribe",
            lua.create_function(move |_, (service, path): (String, String)| {
                sh.core
                    .borrow_mut()
                    .subscribe(&service, &path)
                    .map_err(host_err)
            })?,
        )?;
        let sh = shared.clone();
        let f = fns.clone();
        host.set(
            "call",
            lua.create_function(move |_, (service, method, args, cb): (String, String, LuaValue, Option<Function>)| {
                let args = ToJson { shared: &sh, fns: &RefCell::default(), budget: 10_000, allow_fns: false }.conv(args, None, 0)?;
                let cb_id = cb.map(|cb| {
                    let id = sh.core.borrow_mut().register_fn(FnKind::Handler);
                    f.borrow_mut().insert(id, cb);
                    id
                });
                sh.core.borrow_mut().call(&service, &method, args, cb_id).map_err(host_err)?;
                Ok(())
            })?,
        )?;
        let sh = shared.clone();
        host.set(
            "log",
            lua.create_function(move |_, msg: String| {
                sh.core.borrow_mut().log(msg);
                Ok(())
            })?,
        )?;
        Ok(host)
    }

    fn convert(&self, v: LuaValue) -> mlua::Result<Value> {
        ToJson {
            shared: &self.shared,
            fns: &self.fns,
            budget: self.shared.limits.max_values,
            allow_fns: true,
        }
        .conv(v, None, 0)
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
                "widget must return a component or a function, got {}",
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
