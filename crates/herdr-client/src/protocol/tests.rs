//! The method table against herdr's published schema
//! (`docs/herdr-api-0.9.5.schema.json`), with no server: every name is a
//! method herdr knows, sample params validate against that method's params
//! schema, and each output decodes from answers shaped like the schema's.

use std::collections::BTreeSet;
use std::sync::{LazyLock, Mutex};

use serde_json::{Map, Value, json};

use super::*;
use crate::client::decode;
use crate::events;

const SCHEMA: &str = include_str!("../../../../docs/herdr-api-0.9.5.schema.json");

static DOC: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(SCHEMA).expect("the schema parses"));

/// A validator for the part of the schema at `pointer` (`#/schemas/...`):
/// the whole document with a `$ref` at its root, so the schema's own
/// absolute refs resolve.
fn validator(pointer: &str) -> jsonschema::Validator {
    let mut root = DOC.clone();
    root["$ref"] = json!(pointer);
    jsonschema::validator_for(&root).expect("the schema compiles")
}

fn resolve(pointer: &str) -> &'static Value {
    DOC.pointer(pointer.trim_start_matches('#'))
        .unwrap_or_else(|| panic!("{pointer} is in the schema"))
}

fn variants(pointer: &str) -> &'static [Value] {
    resolve(pointer)["oneOf"]
        .as_array()
        .map_or(&[], Vec::as_slice)
}

/// herdr's request variant for `name`.
fn request(name: &str) -> Option<&'static Value> {
    variants("#/schemas/request")
        .iter()
        .find(|v| v["properties"]["method"]["const"] == name)
}

/// herdr's `result` variant tagged `result_type`.
fn response(result_type: &str) -> Option<&'static Value> {
    variants("#/schemas/success_response/$defs/ResponseResult")
        .iter()
        .find(|v| v["properties"]["type"]["const"] == result_type)
}

/// An instance of `schema`: with every property when `full`, else only the
/// required ones. Picks the first non-null branch of a union and the first
/// value of an enum.
fn example(schema: &Value, full: bool, depth: usize) -> Value {
    assert!(depth < 40, "example recursion in {schema}");
    if let Some(pointer) = schema.get("$ref").and_then(Value::as_str) {
        return example(resolve(pointer), full, depth + 1);
    }
    if let Some(value) = schema.get("const") {
        return value.clone();
    }
    if let Some(value) = schema.get("enum").and_then(|e| e.get(0)) {
        return value.clone();
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(options) = schema.get(key).and_then(Value::as_array) {
            let pick = options
                .iter()
                .find(|o| o.get("type") != Some(&json!("null")))
                .unwrap_or(&options[0]);
            return example(pick, full, depth + 1);
        }
    }
    let ty = match schema.get("type") {
        Some(Value::String(ty)) => ty.as_str(),
        Some(Value::Array(types)) => types
            .iter()
            .filter_map(Value::as_str)
            .find(|ty| *ty != "null")
            .unwrap_or("null"),
        _ if schema.get("properties").is_some() => "object",
        _ => "null",
    };
    match ty {
        "object" => {
            let required: BTreeSet<&str> = schema
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            let mut object = Map::new();
            for (key, property) in schema
                .get("properties")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
            {
                if full || required.contains(key.as_str()) {
                    object.insert(key.clone(), example(property, full, depth + 1));
                }
            }
            Value::Object(object)
        }
        "array" if full => match schema.get("items") {
            Some(items) => json!([example(items, full, depth + 1)]),
            None => json!([]),
        },
        "array" => json!([]),
        "string" => json!("x"),
        "integer" => json!(1),
        "number" => json!(0.5),
        "boolean" => json!(full),
        _ => Value::Null,
    }
}

static PARAMS_CHECKED: Mutex<BTreeSet<&str>> = Mutex::new(BTreeSet::new());
static OUTPUTS_CHECKED: Mutex<BTreeSet<&str>> = Mutex::new(BTreeSet::new());

/// `params` serialize to what herdr's schema accepts for `M`.
fn check_params<M: Method>(params: M::Params) {
    let variant = request(M::NAME).unwrap_or_else(|| panic!("{} is a herdr method", M::NAME));
    let pointer = variant["properties"]["params"]["$ref"]
        .as_str()
        .unwrap_or_else(|| panic!("{} has a params schema", M::NAME));
    let value = serde_json::to_value(&params).expect("params serialize");
    let errors: Vec<String> = validator(pointer)
        .iter_errors(&value)
        .map(|e| format!("{e} at {}", e.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "{} params {value} don't match {pointer}: {errors:?}",
        M::NAME
    );
    PARAMS_CHECKED.lock().unwrap().insert(M::NAME);
}

/// `M::Output` decodes from herdr's `M::RESULT` answer, with only the
/// required fields and with all of them.
fn check_output<M: Method>() {
    let variant = response(M::RESULT)
        .unwrap_or_else(|| panic!("{} answers a known result {:?}", M::NAME, M::RESULT));
    if let Some(field) = M::FIELD {
        assert!(
            variant["properties"].get(field).is_some(),
            "{}'s {:?} result has a {field:?}",
            M::NAME,
            M::RESULT
        );
    }
    let results = validator("#/schemas/success_response/$defs/ResponseResult");
    for full in [false, true] {
        let answer = example(variant, full, 0);
        assert!(
            results.is_valid(&answer),
            "example {answer} is a valid result"
        );
        if let Err(error) = decode::<M>(answer.clone()) {
            panic!("{} output doesn't decode from {answer}: {error}", M::NAME);
        }
    }
    OUTPUTS_CHECKED.lock().unwrap().insert(M::NAME);
}

#[test]
fn every_method_should_be_one_herdr_knows() {
    let unknown: Vec<&str> = ALL
        .iter()
        .copied()
        .filter(|name| request(name).is_none())
        .collect();
    assert!(unknown.is_empty(), "not in the schema: {unknown:?}");
}

fn pane() -> PaneId {
    "w1:p1".into()
}

fn tab() -> TabId {
    "w1:t1".into()
}

fn workspace() -> WorkspaceId {
    "w1".into()
}

#[test]
fn params_should_match_the_schema() {
    use crate::client::{NewTab, NewWorkspace, PaneMoveDestination, PaneSplit, WorktreeParams};

    check_params::<method::Ping>(NoParams {});
    check_params::<method::SessionSnapshot>(NoParams {});
    check_params::<method::EventsSubscribe>(Subscribe {
        subscriptions: events::subscriptions(&[pane()]),
    });
    check_params::<method::LayoutExport>(LayoutTarget { tab_id: None });
    check_params::<method::LayoutExport>(LayoutTarget {
        tab_id: Some(tab()),
    });
    check_params::<method::LayoutSetSplitRatio>(SplitRatio {
        tab_id: tab(),
        path: vec![true, false],
        ratio: 0.4,
    });
    let mut split = PaneSplit::new(Some(pane()), SplitDirection::Right);
    split.focus = true;
    split.right_click = Some(RightClickTarget::Pane);
    split.env.insert("A".into(), "b".into());
    check_params::<method::PaneSplit>(split);
    check_params::<method::PaneSplit>(PaneSplit::new(None, SplitDirection::Down));
    check_params::<method::PaneSwap>(PaneSwapParams::Panes {
        source_pane_id: pane(),
        target_pane_id: "w1:p2".into(),
    });
    check_params::<method::PaneSwap>(PaneSwapParams::Toward {
        pane_id: pane(),
        direction: PaneDirection::Left,
    });
    for destination in [
        PaneMoveDestination::Tab {
            tab_id: tab(),
            split: SplitDirection::Right,
            target_pane_id: Some(pane()),
            ratio: Some(0.5),
        },
        PaneMoveDestination::NewTab {
            workspace_id: Some(workspace()),
            label: None,
        },
        PaneMoveDestination::NewWorkspace {
            label: Some("l".into()),
            tab_label: None,
        },
    ] {
        check_params::<method::PaneMove>(PaneMoveParams {
            pane_id: pane(),
            destination,
            focus: true,
        });
    }
    check_params::<method::PaneClose>(PaneTarget { pane_id: pane() });
    check_params::<method::PaneFocus>(PaneTarget { pane_id: pane() });
    check_params::<method::PaneGet>(PaneTarget { pane_id: pane() });
    check_params::<method::PaneList>(InWorkspace {
        workspace_id: Some(workspace()),
    });
    check_params::<method::PaneRename>(PaneRenameParams {
        pane_id: pane(),
        label: None,
    });
    check_params::<method::PaneZoom>(PaneZoomParams {
        pane_id: pane(),
        mode: ZoomMode::Toggle,
    });
    check_params::<method::PaneInputSet>(PaneInputSetParams {
        pane_id: pane(),
        right_click: RightClickTarget::Pane,
    });
    check_params::<method::PaneSendInput>(PaneSendInputParams {
        pane_id: pane(),
        text: "ls\r".into(),
        keys: vec!["Enter".into()],
    });
    check_params::<method::PaneReportAgent>(PaneReportAgentParams {
        pane_id: pane(),
        source: "test".into(),
        agent: "claude".into(),
        state: ReportedState::Working,
    });
    check_params::<method::PaneRead>(PaneReadParams {
        pane_id: pane(),
        source: ReadSource::Recent,
        lines: Some(40),
    });
    check_params::<method::PaneSelectionRead>(PaneSelectionReadParams {
        pane_id: pane(),
        anchor: PaneTextPoint { row: 0, col: 0 },
        cursor: PaneTextPoint { row: 9, col: 79 },
    });
    check_params::<method::PaneLayoutGet>(PaneTarget { pane_id: pane() });

    check_params::<method::TabCreate>(NewTab {
        workspace_id: Some(workspace()),
        label: Some("t".into()),
        focus: true,
        ..NewTab::default()
    });
    check_params::<method::TabList>(InWorkspace::default());
    check_params::<method::TabGet>(TabTarget { tab_id: tab() });
    check_params::<method::TabFocus>(TabTarget { tab_id: tab() });
    check_params::<method::TabRename>(TabRenameParams {
        tab_id: tab(),
        label: "t".into(),
    });
    check_params::<method::TabMove>(TabMoveParams {
        tab_id: tab(),
        insert_index: 2,
    });
    check_params::<method::TabClose>(TabTarget { tab_id: tab() });

    check_params::<method::WorkspaceCreate>(NewWorkspace {
        cwd: Some("/tmp".into()),
        label: Some("w".into()),
        focus: true,
        ..NewWorkspace::default()
    });
    check_params::<method::WorkspaceList>(NoParams {});
    check_params::<method::WorkspaceGet>(WorkspaceTarget {
        workspace_id: workspace(),
    });
    check_params::<method::WorkspaceFocus>(WorkspaceTarget {
        workspace_id: workspace(),
    });
    check_params::<method::WorkspaceRename>(WorkspaceRenameParams {
        workspace_id: workspace(),
        label: "w".into(),
    });
    check_params::<method::WorkspaceMove>(WorkspaceMoveParams {
        workspace_id: workspace(),
        insert_index: 0,
    });
    check_params::<method::WorkspaceMoveBlock>(WorkspaceMoveBlockParams {
        workspace_ids: vec![workspace(), "w2".into()],
        before_workspace_id: Some("w3".into()),
    });
    check_params::<method::WorkspaceClose>(WorkspaceCloseParams {
        workspace_id: workspace(),
        close_group: true,
    });

    check_params::<method::WorktreeList>(WorktreeListParams {
        workspace_id: Some(workspace()),
        cwd: None,
    });
    let worktree = WorktreeParams {
        workspace_id: Some(workspace()),
        branch: Some("b".into()),
        path: Some("/r/b".into()),
        label: Some("b".into()),
        focus: true,
        trust_repository: Some(true),
        ..WorktreeParams::default()
    };
    check_params::<method::WorktreeCreate>(WorktreeParams {
        base: Some("main".into()),
        ..worktree.clone()
    });
    check_params::<method::WorktreeOpen>(worktree);
    check_params::<method::WorktreeRemove>(WorktreeRemoveParams {
        workspace_id: workspace(),
        force: false,
    });

    check_params::<method::AgentList>(NoParams {});
    check_params::<method::AgentGet>(AgentTarget {
        target: "w1:p1".into(),
    });
    check_params::<method::AgentFocus>(AgentTarget {
        target: "claude".into(),
    });

    check_params::<method::PluginActionList>(PluginActionListParams::default());
    check_params::<method::PluginActionInvoke>(PluginActionInvokeParams {
        plugin_id: Some("p".into()),
        action_id: "a".into(),
        context: Some(PluginInvocationContext {
            invocation_source: Some("command_palette".into()),
            focused_pane_id: Some(pane()),
            focused_pane_status: Some(AgentStatus::Working),
            tab_id: Some(tab()),
            workspace_id: Some(workspace()),
            ..PluginInvocationContext::default()
        }),
    });

    check_params::<method::ServerSshAgentRegister>(SshAgentRegisterParams {
        socket_path: "/tmp/agent.sock".into(),
    });
    check_params::<method::ServerReloadConfig>(NoParams {});
    check_params::<method::NotificationShow>(NotificationShowParams {
        title: "t".into(),
        body: Some("b".into()),
    });

    let checked = PARAMS_CHECKED.lock().unwrap();
    let missing: Vec<&&str> = ALL.iter().filter(|name| !checked.contains(*name)).collect();
    assert!(missing.is_empty(), "no sample params for {missing:?}");
}

#[test]
fn outputs_should_decode_from_schema_shaped_answers() {
    check_output::<method::Ping>();
    check_output::<method::SessionSnapshot>();
    check_output::<method::EventsSubscribe>();
    check_output::<method::LayoutExport>();
    check_output::<method::LayoutSetSplitRatio>();
    check_output::<method::PaneSplit>();
    check_output::<method::PaneSwap>();
    check_output::<method::PaneMove>();
    check_output::<method::PaneClose>();
    check_output::<method::PaneFocus>();
    check_output::<method::PaneGet>();
    check_output::<method::PaneList>();
    check_output::<method::PaneRename>();
    check_output::<method::PaneZoom>();
    check_output::<method::PaneInputSet>();
    check_output::<method::PaneSendInput>();
    check_output::<method::PaneReportAgent>();
    check_output::<method::PaneRead>();
    check_output::<method::PaneSelectionRead>();
    check_output::<method::PaneLayoutGet>();
    check_output::<method::TabCreate>();
    check_output::<method::TabList>();
    check_output::<method::TabGet>();
    check_output::<method::TabFocus>();
    check_output::<method::TabRename>();
    check_output::<method::TabMove>();
    check_output::<method::TabClose>();
    check_output::<method::WorkspaceCreate>();
    check_output::<method::WorkspaceList>();
    check_output::<method::WorkspaceGet>();
    check_output::<method::WorkspaceFocus>();
    check_output::<method::WorkspaceRename>();
    check_output::<method::WorkspaceMove>();
    check_output::<method::WorkspaceMoveBlock>();
    check_output::<method::WorkspaceClose>();
    check_output::<method::WorktreeList>();
    check_output::<method::WorktreeCreate>();
    check_output::<method::WorktreeOpen>();
    check_output::<method::WorktreeRemove>();
    check_output::<method::AgentList>();
    check_output::<method::AgentGet>();
    check_output::<method::AgentFocus>();
    check_output::<method::PluginActionList>();
    check_output::<method::PluginActionInvoke>();
    check_output::<method::ServerSshAgentRegister>();
    check_output::<method::ServerReloadConfig>();
    check_output::<method::NotificationShow>();

    let checked = OUTPUTS_CHECKED.lock().unwrap();
    let missing: Vec<&&str> = ALL.iter().filter(|name| !checked.contains(*name)).collect();
    assert!(missing.is_empty(), "no output check for {missing:?}");
}
