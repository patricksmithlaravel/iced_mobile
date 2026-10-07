//! Plans (design §1 principle 1): every command builds a list of steps,
//! each with its argv, environment delta, working directory, timeout and
//! gates. `--dry-run` (and `icm print plan <cmd…>`) prints the plan and
//! changes nothing; otherwise [`Plan::execute`] runs it, one step log each.

use crate::catalogue::CheckId;
use crate::context::Ctx;
use crate::error::Result;
use crate::process::{Cmd, Outcome};
use serde_json::{Map, Value, json};

/// What a step does.
#[derive(Clone, Debug)]
pub enum Action {
    /// Run a process.
    Exec(Cmd),
    /// Work icm does itself (generating files, parsing, waiting).
    Internal(String),
}

/// One step.
#[derive(Clone, Debug)]
pub struct Step {
    /// A dotted name, e.g. `cargo.build`.
    pub name: String,
    /// What it does.
    pub action: Action,
    /// The checks run on its output.
    pub gates: Vec<CheckId>,
    /// The error id when the process fails.
    pub on_fail: CheckId,
}

impl Step {
    /// A process step.
    pub fn exec(name: &str, cmd: Cmd) -> Step {
        Step {
            name: name.to_string(),
            action: Action::Exec(cmd),
            gates: Vec::new(),
            on_fail: CheckId::ToolFailed,
        }
    }

    /// An internal step.
    pub fn internal(name: &str, description: &str) -> Step {
        Step {
            name: name.to_string(),
            action: Action::Internal(description.to_string()),
            gates: Vec::new(),
            on_fail: CheckId::InternalBug,
        }
    }

    /// Adds a gate.
    pub fn gate(mut self, id: CheckId) -> Step {
        self.gates.push(id);
        self
    }

    /// Sets the error id for a failed process.
    pub fn on_fail(mut self, id: CheckId) -> Step {
        self.on_fail = id;
        self
    }

    /// The step as plan JSON.
    pub fn to_json(&self) -> Value {
        let mut value = json!({
            "name": self.name,
            "gates": self.gates.iter().map(|g| g.id()).collect::<Vec<_>>(),
            "on_fail": self.on_fail.id(),
        });
        match &self.action {
            Action::Exec(cmd) => {
                let env: Map<String, Value> = cmd
                    .display_env()
                    .into_iter()
                    .map(|(k, v)| (k, Value::String(v)))
                    .collect();
                value["kind"] = json!("exec");
                value["argv"] = json!(cmd.display_argv());
                value["display"] = json!(cmd.display());
                value["env"] = Value::Object(env);
                value["cwd"] = json!(cmd.cwd.as_deref().map(crate::paths::display));
                value["timeout_ms"] = json!(cmd.timeout.map(|t| t.as_millis() as u64));
            }
            Action::Internal(description) => {
                value["kind"] = json!("internal");
                value["display"] = json!(format!("(icm) {description}"));
            }
        }
        value
    }
}

/// A list of steps.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    /// The steps, in order.
    pub steps: Vec<Step>,
}

impl Plan {
    /// An empty plan.
    pub fn new() -> Plan {
        Plan::default()
    }

    /// Appends a step.
    pub fn push(&mut self, step: Step) {
        self.steps.push(step);
    }

    /// The plan as JSON.
    pub fn to_json(&self) -> Value {
        Value::Array(self.steps.iter().map(Step::to_json).collect())
    }

    /// Reports the plan: a `plan` event (`PLAN` lines in human mode) and
    /// the result's `plan` field. Used by `--dry-run`.
    pub fn report(&self, ctx: &Ctx) {
        let steps = self.to_json();
        ctx.rep.emit(json!({"type": "plan", "steps": steps}));
        ctx.rep.set("plan", steps);
        ctx.rep.set("dry_run", Value::Bool(true));
    }

    /// Runs the plan: process steps through the runner (a step log each;
    /// a failure stops the plan with the step's `on_fail` id), internal
    /// steps through `internal`. Returns each process step's outcome.
    pub fn execute(
        &self,
        ctx: &Ctx,
        mut internal: impl FnMut(&Step) -> Result<()>,
    ) -> Result<Vec<Option<Outcome>>> {
        let mut outcomes = Vec::with_capacity(self.steps.len());
        for step in &self.steps {
            match &step.action {
                Action::Exec(cmd) => {
                    let outcome = ctx.step(&step.name, cmd)?;
                    if !outcome.success() {
                        return Err(ctx.step_failure(&step.name, step.on_fail, &outcome));
                    }
                    outcomes.push(Some(outcome));
                }
                Action::Internal(_) => {
                    // Takes a number too, so log names match plan positions.
                    let _ = ctx.rep.step_log(&step.name);
                    let started = std::time::Instant::now();
                    let result = internal(step);
                    ctx.rep.step_end_internal(
                        &step.name,
                        result.is_ok(),
                        started.elapsed().as_millis() as u64,
                    );
                    result?;
                    outcomes.push(None);
                }
            }
        }
        Ok(outcomes)
    }
}
