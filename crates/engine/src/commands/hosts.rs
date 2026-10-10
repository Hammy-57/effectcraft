//! `hosts.list`: the EffectCraft processes that have projects open ([`crate::hosts`]).

use serde_json::{Value, json};

use super::CommandSpec;
use crate::{Result, Session, query};

fn list(_: &mut Session, _: &Value) -> Result<Value> {
    Ok(json!({"hosts": crate::hosts::list()}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![query!(
        "hosts.list",
        "List Hosts",
        "{} → {hosts: [{pid, kind: app|mcp, port (join with --bridge PORT), project, me}]}: every running EffectCraft and the project it has open",
        list
    )]
}
