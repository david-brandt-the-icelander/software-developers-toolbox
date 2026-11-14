// The script's handlers and the dispatch of protocol messages to them.
use crate::call::Call;
use crate::error::{Error, Result};
use crate::json::Json;

// Handles json-command: JSON in, JSON out.
pub type CommandFn = Box<dyn Fn(&mut Call, Json) -> Result<Json>>;
// Handles one service-call operation: params and body in, JSON out.
pub type ServiceFn = Box<dyn Fn(&mut Call, &Json, Json) -> Result<Json>>;

// The handlers a script registers before calling zazz::serve.
#[derive(Default)]
pub struct Handlers {
    command: Option<CommandFn>,
    openapi: Option<Box<dyn Fn() -> Json>>,
    services: Vec<(String, ServiceFn)>,
}

impl Handlers {
    // No handlers yet.
    pub fn new() -> Handlers {
        Handlers::default()
    }

    // Registers the required json-command handler.
    pub fn json_command(mut self, f: impl Fn(&mut Call, Json) -> Result<Json> + 'static) -> Handlers {
        self.command = Some(Box::new(f));
        self
    }

    // Registers the OpenAPI document that describes the script's operations.
    pub fn openapi(mut self, f: impl Fn() -> Json + 'static) -> Handlers {
        self.openapi = Some(Box::new(f));
        self
    }

    // Registers a service-call operation by its operationId.
    pub fn service(
        mut self,
        op: &str,
        f: impl Fn(&mut Call, &Json, Json) -> Result<Json> + 'static,
    ) -> Handlers {
        self.services.push((op.to_string(), Box::new(f)));
        self
    }

    // The protocol messages these handlers answer.
    pub fn messages(&self) -> Vec<&'static str> {
        let mut m = Vec::new();
        if self.command.is_some() {
            m.push("json-command");
        }
        if self.openapi.is_some() {
            m.push("openapi");
        }
        if !self.services.is_empty() {
            m.push("service-call");
        }
        m
    }

    // Runs the json-command handler.
    pub fn command(&self, call: &mut Call, input: Json) -> Result<Json> {
        let f = self
            .command
            .as_ref()
            .ok_or_else(|| Error::unsupported("this script has no json-command handler"))?;
        f(call, input)
    }

    // Returns the OpenAPI document.
    pub fn openapi_doc(&self) -> Result<Json> {
        self.openapi
            .as_ref()
            .map(|f| f())
            .ok_or_else(|| Error::unsupported("this script has no openapi document"))
    }

    // Runs one service-call operation.
    pub fn service_call(&self, call: &mut Call, op: &str, params: &Json, body: Json) -> Result<Json> {
        let f = self.services.iter().find(|(name, _)| name == op).map(|(_, f)| f);
        let f = f.ok_or_else(|| Error::not_found(&format!("unknown operation '{op}'")))?;
        f(call, params, body)
    }

    // Dispatches a json-command, openapi or service-call message.
    pub fn dispatch(&self, call: &mut Call, msg: &Json) -> Result<Json> {
        match crate::proto::kind(msg) {
            "json-command" => self.command(call, msg["input"].clone()),
            "openapi" => self.openapi_doc(),
            "service-call" => {
                self.service_call(call, msg.need_str("operation")?, &msg["params"], msg["body"].clone())
            }
            other => Err(Error::unsupported(&format!("unknown message '{other}'"))),
        }
    }
}

// Reads the mode of a call message.
pub fn mode_of(msg: &Json) -> crate::call::Mode {
    if msg.str_or("mode", "agent") == "human" {
        crate::call::Mode::Human
    } else {
        crate::call::Mode::Agent
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::call::{Mode, NoHttp};

    // Builds a call over empty streams.
    fn call() -> Call {
        Call::real(
            1,
            Mode::Agent,
            ".".into(),
            (Box::new(&b""[..]), Box::new(Vec::new()), Box::new(Vec::new())),
            Box::new(NoHttp),
        )
    }

    // Checks dispatch to each handler kind and the error cases.
    #[test]
    fn test_dispatch() {
        let h = Handlers::new()
            .json_command(|_, input| Ok(crate::json!({"echo": input})))
            .openapi(|| crate::json!({"openapi": "3.1.0"}))
            .service("sum", |_, params, _| Ok(Json::Int(params.need_i64("a")? + 1)));
        assert_eq!(h.messages(), vec!["json-command", "openapi", "service-call"]);
        let mut c = call();
        let out = h.dispatch(&mut c, &crate::json!({"t": "json-command", "input": 5})).expect("cmd");
        assert_eq!(out["echo"], Json::Int(5));
        assert_eq!(
            h.dispatch(&mut c, &crate::json!({"t": "openapi"})).expect("doc")["openapi"].str(),
            Some("3.1.0")
        );
        let sc = crate::json!({"t": "service-call", "operation": "sum", "params": {"a": 1}});
        assert_eq!(h.dispatch(&mut c, &sc).expect("svc"), Json::Int(2));
        let bad = crate::json!({"t": "service-call", "operation": "nope"});
        assert_eq!(h.dispatch(&mut c, &bad).unwrap_err().code, "not_found");
        assert_eq!(h.dispatch(&mut c, &crate::json!({"t": "x"})).unwrap_err().code, "unsupported");
        let empty = Handlers::new();
        assert!(empty.messages().is_empty());
        assert_eq!(
            empty.dispatch(&mut c, &crate::json!({"t": "json-command"})).unwrap_err().code,
            "unsupported"
        );
        assert_eq!(empty.dispatch(&mut c, &crate::json!({"t": "openapi"})).unwrap_err().code, "unsupported");
    }

    // Checks the mode is read from the message.
    #[test]
    fn test_mode_of() {
        assert_eq!(mode_of(&crate::json!({"mode": "human"})), Mode::Human);
        assert_eq!(mode_of(&crate::json!({})), Mode::Agent);
    }
}
