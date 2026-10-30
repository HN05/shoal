//! Every request's JSON, as clients and daemons of this protocol version
//! write it.
use std::collections::BTreeSet;

use serde::de::{self, Deserializer, Visitor};

use super::*;

/// One request of every variant, spelled as the daemon reads and writes it.
const REQUESTS: &[&str] = &[
    // Daemon administration.
    r#""status""#,
    r#""reload_config""#,
    // Repositories.
    r#""list_repositories""#,
    r#"{"register_repository":{"source":"app","name":null,"path":null}}"#,
    r#"{"register_contribution":{"source":"https://forge.example/team/app","name":"app"}}"#,
    r#"{"rename_repository":{"repository":{"value":".","path":"/work/app"},"name":"web"}}"#,
    r#"{"local_config_path":{"repository":"app"}}"#,
    r#"{"edit_local_config":{"repository":"app","changes":[{"key":"setup.command","value":null}]}}"#,
    r#"{"remove_repository":{"repository":"app"}}"#,
    r#"{"plan_repository_moves":{"root":"/work","regular_dir":false}}"#,
    r#"{"move_repositories":{"regular_dir":true}}"#,
    r#"{"sync_repository":{"repository":"app"}}"#,
    r#"{"list_branches":{"repository":"app"}}"#,
    r#"{"kept_branches":{"repository":"app"}}"#,
    r#"{"delete_kept_branches":{"branches":[{"repository_id":"r1","branch":"fix","head":"abc"}]}}"#,
    r#"{"agent_environments":{"repository_id":"r1"}}"#,
    r#"{"forget_agent_environments":{"repository_id":"r1"}}"#,
    r#"{"keep_repository_agent_environment":{"repository":"app","environment":{"CODEX_HOME":"/codex"}}}"#,
    // Workspaces.
    r#"{"open_branch":{"path":null,"repository":"app","branch":"fix","git_profile":null,"base":"main","chat":null}}"#,
    r#"{"create_workspace":{"path":"/work/fix","repository":"app","name":"fix","base":null,"git_profile":"work","chat":"c1"}}"#,
    r#"{"reopen_workspace":{"workspace":"fix"}}"#,
    r#"{"split_workspace":{"workspace":"fix","name":"part","base":null}}"#,
    r#"{"check_creation":{"repository":"app","branch":"fix","existing":null,"base":null,"git_profile":null,"path":null}}"#,
    r#"{"check_dependencies":{"repository":"app","after":[{"work":"fix"},{"issue":"https://forge.example/team/app/issues/1"}]}}"#,
    r#"{"adopt_workspace":{"repository":"app","path":"/work/fix","copy":false}}"#,
    r#"{"rename_workspace":{"workspace":"fix","branch":"fix-2"}}"#,
    r#""list_workspaces""#,
    r#""untracked_worktrees""#,
    r#"{"find_workspaces":{"target":{"item":{"kind":"pr","input":"7"}}}}"#,
    r#"{"set_workspace_title":{"workspace":"fix","title":"Login retries"}}"#,
    r#"{"workspace_places":{"workspace":"fix"}}"#,
    r#"{"set_base_workspace":{"workspace":"part","base":"fix"}}"#,
    r#"{"inspect_workspace":{"workspace":"fix"}}"#,
    r#"{"verify_workspace":{"workspace":"fix","on_branch":true}}"#,
    r#"{"fast_forward_workspace":{"workspace":"fix","branch":"main"}}"#,
    r#"{"workspace_env":{"workspace":"fix"}}"#,
    r#"{"revoke_workspace_env":{"workspace":"fix","token":"t1"}}"#,
    r#"{"workspace_status":{"workspace":"fix"}}"#,
    r#"{"diff_base":{"workspace":"fix"}}"#,
    r#"{"conflicts":{"workspace":"fix","target":null}}"#,
    r#"{"workspace_hook":{"workspace":"fix","kind":"post_setup"}}"#,
    r#"{"layered_config":{"target":{"workspace":"fix"}}}"#,
    r#"{"keep_agent_environment":{"workspace":"fix","environment":{}}}"#,
    r#"{"close_owned_place":{"workspace":"fix","place":{"host":"rex","place":"block:1"}}}"#,
    r#"{"place_ownership":{"workspace":"fix","place":{"host":"herdr","place":"w1:p1"}}}"#,
    r#"{"record_owned_place":{"workspace":"fix","origin":{"host":"rex","place":"block:1","owned":true}}}"#,
    r#""owned_places""#,
    r#"{"hold_acquire":{"workspace":"fix","name":"review","reason":null}}"#,
    r#"{"hold_release":{"workspace":"fix","name":"review"}}"#,
    r#"{"hold_list":{"workspace":"fix"}}"#,
    r#"{"watch_workspace_events":{"since":4,"follow":true}}"#,
    // Removal and restore.
    r#""stop_targets""#,
    r#"{"stop_workspace":{"workspace":"fix","acknowledge_unproven":false}}"#,
    r#"{"check_removal":{"workspace":"fix","include_changes":true}}"#,
    r#"{"remove_workspace":{"workspace":"fix","unsaved":"all"}}"#,
    r#"{"cleanup":{"dry_run":true,"workspaces":"all","caches":"none"}}"#,
    r#""removed_workspaces""#,
    r#"{"restore_workspace":{"workspace":"fix"}}"#,
    r#"{"export_workspace":{"workspace":"fix","unsaved":{"confirmed":{"files":true,"detached_head":null,"files_restorable":false}},"file":"/work/fix.shoal"}}"#,
    r#"{"import_workspace":{"repository":"app","bundle":null,"prerequisite":null,"workspace":{"workspace_id":"w1","repository_id":"r1","name":"fix","checkpoint":{"workspace_id":"w1","repository_id":"r1","number":1,"cause":"removal","branch":"fix","head":"abc","commit":"abc","files_omitted":false,"created_at":1,"expires_at":2},"base_ref":null,"base_commit":null,"links":{"issue":null,"prs":[],"opened":[],"related":[]},"cause":"manual","removed_at":1,"expires_at":2}}}"#,
    r#"{"checkpoint":{"workspace":"fix","name":"before rebase"}}"#,
    r#"{"checkpoints":{"workspace":"fix","after":null}}"#,
    r#"{"restore_checkpoint":{"workspace":"fix","checkpoint":2}}"#,
    // Assignments and agents.
    r#"{"workspace_done":{"workspace":"fix","cleanup":true}}"#,
    r#"{"workspace_undone":{"workspace":"fix"}}"#,
    r#"{"discard_stopped_work":{"workspace":"part","execution":null}}"#,
    r#"{"mark_ready":{"workspace":"fix","selection":{"kind":"pr","input":null}}}"#,
    r#"{"clear_ready":{"workspace":"fix","selection":{"kind":null,"input":null},"draft":true}}"#,
    r#"{"set_agent_state":{"workspace":"fix","state":"idle","session":"s1","turn_end":{"prompt_cache_secs":3600}}}"#,
    r#"{"start_woken_turn":{"workspace":"fix"}}"#,
    r#"{"report_turn_failure":{"workspace":"fix","failure":"rate_limited","turn_end":{"prompt_cache_secs":300}}}"#,
    r#"{"report_proxy_retry":{"workspace":"fix","execution":"e1","retry":{"failure":"overloaded","attempt":2,"next_at":1700000000}}}"#,
    r#"{"report_upstream_reset":{"workspace":"fix","execution":"e1","resets_at":1700000000}}"#,
    r#"{"report_attached":{"workspace":"fix","execution":"e1","attached":true}}"#,
    r#"{"await_agent_wake":{"workspace":"fix","due":true,"interrupted":"e1"}}"#,
    r#"{"workspace_agents":{"workspace":"fix"}}"#,
    r#"{"interrupt_turn":{"workspace":"fix","execution":"e1"}}"#,
    // Executions.
    r#"{"execute":{"workspace":"fix","wrapper":{"pid":2,"birth":"b"},"kind":"command","agent":null,"recover":false,"reattach":false,"queue_entry":null}}"#,
    r#"{"reattach":{"execution":"e1","wrapper":{"pid":2,"birth":"b"},"child":null,"group_id":2,"scope_token":"t1","agent":null,"recover":false,"running_ms":0}}"#,
    r#""check_landing""#,
    r#""list_agents""#,
    r#"{"restart_agent":{"execution":"e1"}}"#,
    // Chats.
    r#"{"start_chat":{"repository":"app","name":"app-chat","agent":"claude","pid":4}}"#,
    r#"{"move_chat":{"chat":"c1","pid":5}}"#,
    r#""list_chats""#,
    r#"{"rename_chat":{"chat":"app-chat","name":"topic"}}"#,
    r#"{"check_chat_name":{"name":"app-chat"}}"#,
    r#"{"free_chat_name":{"name":"app-chat"}}"#,
    r#"{"chat_updates":{"chat":"c1"}}"#,
    r#"{"wait_chat_messages":{"chat":"c1"}}"#,
    r#"{"acknowledge_chat_messages":{"chat":"c1","ids":[1]}}"#,
    // The agent queue.
    r#"{"queue_add":{"request":{"repository":"app","name":"fix","agent":"claude","issue_title":null,"plan":"{}","env":{},"now":false,"workspace":null,"after":[]}}}"#,
    r#""list_queue""#,
    r#"{"queued_plan":{"entry":"q1"}}"#,
    r#"{"queue_remove":{"entry":"q1"}}"#,
    r#"{"queue_start":{"entry":"q1"}}"#,
    r#"{"queue_move":{"entry":"q1","place":{"position":2}}}"#,
    r#"{"queue_hold":{"entry":"q1"}}"#,
    r#"{"queue_attach":{"entry":"q1","workspace":"fix"}}"#,
    r#"{"queue_fail":{"entry":"q1","error":"no agent"}}"#,
    r#"{"queue_restore":{"workspace":"fix","execution":"e1","env":{}}}"#,
    // Demos.
    r#"{"list_demos":{"workspace":null}}"#,
    r#"{"demo_progress":{"workspace":"fix","launch":"l1"}}"#,
    r#"{"stop_demos":{"workspace":"fix","name":"web"}}"#,
    // Issues and PRs.
    r#"{"set_issue":{"workspace":"fix","url":"https://forge.example/team/app/issues/1","title":"Fix"}}"#,
    r#"{"link_item":{"workspace":"fix","kind":"issue","url":"1","relation":"related"}}"#,
    r#"{"unlink_items":{"workspace":"fix","relation":"assigned","kind":null,"url":null}}"#,
    r#"{"set_pr":{"workspace":"fix","url":"7","clear":false}}"#,
    r#"{"pr_wait":{"workspace":"fix","timeout_secs":60}}"#,
    r#"{"watch_items":{"workspace":"fix","selection":{"kind":null,"input":"7"},"timeout_secs":null}}"#,
    r#"{"acknowledge_pr_updates":{"workspace":"fix","deliveries":["d1"],"messages":[3],"attached":true}}"#,
    r#"{"open_pull":{"workspace":"fix","options":{"title":null,"body":null,"base":null,"draft":true,"labels":[],"reviewers":[],"attachments":[]}}}"#,
    r#"{"revert_pull":{"workspace":"fix","item":"42","options":{"title":null,"body":null,"base":null,"draft":null,"labels":[],"reviewers":[],"attachments":[]}}}"#,
    r#"{"merged_pull":{"workspace":"fix","item":null}}"#,
    r#"{"open_issue":{"workspace":"fix","issue":{"title":"Bug","body":"","labels":[],"attachments":[]},"repository":null,"link":false}}"#,
    r#"{"item_action":{"workspace":"fix","kind":"pr","item":"7","action":{"action":"close"}}}"#,
    r#"{"list_items":{"workspace":"fix","kind":"issue","search":{"state":"open","labels":[],"author":null,"text":"crash","limit":30},"repository":null}}"#,
    r#"{"forge_api":{"workspace":"fix","request":{"method":"GET","path":"repos/{owner}/{repo}","body":null},"repository":null}}"#,
    r#"{"select_items":{"workspace":"fix","selection":{"kind":"issue","input":null}}}"#,
    r#"{"forge_type":{"host":"forge.example","scheme":"https"}}"#,
    // CI.
    r#"{"check_log":{"workspace":"fix","selection":{"kind":"pr","input":null},"check":"rust"}}"#,
    r#"{"ci_action":{"workspace":"fix","item":null,"action":"rerun","checks":["rust"]}}"#,
    r#"{"ci_run_action":{"workspace":"fix","run":"12","action":"cancel","failed":false}}"#,
    r#"{"view_ci_run":{"workspace":"fix","run":"12"}}"#,
    r#"{"ci_run_jobs":{"workspace":"fix","run":"12"}}"#,
    r#"{"ci_run_job_log":{"workspace":"fix","run":"12","job":{"name":"rust"}}}"#,
    r#"{"list_ci_runs":{"workspace":"fix","filter":{"branch":null,"workflow":"ci.yml","limit":10}}}"#,
    r#"{"start_ci_run":{"workspace":"fix","workflow":null,"branch":null,"inputs":{"level":"high"}}}"#,
    r#"{"approve_ci_runs":{"workspace":"fix","run":null,"pr":"7"}}"#,
    // Reconciliation.
    r#"{"reconcile":{"workspace":null,"options":{"repair":true,"stop":false,"acknowledge_stopped":false,"reclaim":false}}}"#,
    // Notifications and messages.
    r#"{"list_notifications":{"unread_only":true,"limit":20}}"#,
    r#"{"mark_notifications_read":{"ids":[1,2]}}"#,
    r#""clear_notifications""#,
    r#"{"watch_notifications":{"keep_unread":false,"after":3}}"#,
    r#"{"send_message":{"workspace":"fix","message":"done"}}"#,
    r#"{"withdraw_message":{"workspace":"fix","id":4}}"#,
    r#"{"send_agent_message":{"workspace":"part","message":"rebase","attachments":[{"path":"/work/log.txt","title":"Crash log"},{"path":"/work/shot.png"}]}}"#,
    r#"{"send_repository_agent_message":{"repository":"app","message":"rebase"}}"#,
    r#"{"agent_messages":{"workspace":"fix"}}"#,
    r#"{"mark_agent_messages_delivered":{"workspace":"fix","ids":[5]}}"#,
    r#"{"agent_updates":{"workspace":"fix"}}"#,
    r#"{"agent_session":{"workspace":"fix"}}"#,
    // Questions.
    r#"{"ask_question":{"workspace":"fix","ask":{"question":"Which?","recommendation":"A","choices":["A","B"]}}}"#,
    r#"{"list_questions":{"workspace":"fix"}}"#,
    r#"{"show_question":{"id":6}}"#,
    r#"{"withdraw_question":{"workspace":"fix","id":6}}"#,
    r#"{"answer_question":{"id":6,"answer":"A"}}"#,
    r#"{"accept_question_recommendation":{"id":6,"shown":null}}"#,
    r#"{"await_answer":{"workspace":"fix","id":6,"ask":{"question":"Which?","recommendation":"A","reason":"Simpler"}}}"#,
    // Approvals.
    r#"{"list_approvals":{"workspace":null}}"#,
    r#"{"show_approval":{"id":7}}"#,
    r#"{"decide_approval":{"id":7,"approve":true,"details":{"reason":"later"}}}"#,
    r#"{"escalate_approval":{"workspace":"fix","id":7}}"#,
    r#"{"withdraw_approval":{"workspace":"fix","id":7}}"#,
    // Ports.
    r#"{"port_acquire":{"workspace":"fix","name":"web","request":{"port":8080,"env_var":null,"reason":null,"on_conflict":null}}}"#,
    r#"{"port_release":{"workspace":"fix","name":"web"}}"#,
    r#"{"port_overview":{"workspace":"fix"}}"#,
    r#"{"port_catalog":{"workspace":"fix"}}"#,
    // Secrets.
    r#"{"secret_acquire":{"workspace":"fix","name":"TOKEN","request":{"reason":"deploy","source":null}}}"#,
    r#"{"secret_release":{"workspace":"fix","name":"TOKEN"}}"#,
    r#"{"secret_catalog":{"workspace":"fix"}}"#,
    // Cooperative resources.
    r#"{"resource_acquire":{"workspace":"fix","request":{"mode":"read","kind":null,"pool":"devices","name":"default","resource":null,"reason":null,"queue":true}}}"#,
    r#"{"resource_release":{"workspace":"fix","pool":"devices","name":"default"}}"#,
    r#"{"resource_overview":{"workspace":"fix"}}"#,
    r#"{"resource_catalog":{"workspace":"fix"}}"#,
    // Simulators.
    r#"{"sim_catalog":{"workspace":"fix"}}"#,
    r#"{"sim_overview":{"workspace":null}}"#,
    r#"{"sim_list":{"workspace":"fix"}}"#,
    r#"{"sim_acquire":{"workspace":"fix","request":{"request_id":"u1","clean":false,"name":"default","profile":null,"device":null,"runtime":null,"reason":null,"queue":false}}}"#,
    r#"{"sim_release":{"workspace":"fix","name":"default"}}"#,
    r#"{"sim_history":{"workspace":null,"limit":20,"before":null}}"#,
];

#[test]
fn every_request_keeps_its_json() {
    let mismatches: Vec<String> = REQUESTS
        .iter()
        .filter_map(|wire| match serde_json::from_str::<Method>(wire) {
            Ok(method) => {
                let written = serde_json::to_string(&method).unwrap_or_default();
                (written != *wire).then(|| format!("{wire}\n  written as {written}"))
            }
            Err(error) => Some(format!("{wire}\n  unreadable: {error}")),
        })
        .collect();
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn every_request_has_its_json_above() {
    let sampled: BTreeSet<String> = REQUESTS
        .iter()
        .filter_map(|wire| serde_json::from_str::<Method>(wire).ok())
        .map(|method| method.name())
        .collect();
    let variants: BTreeSet<String> = [
        variants::<Reattachment>(),
        variants::<DaemonMethod>(),
        variants::<RepositoryMethod>(),
        variants::<WorkspaceMethod>(),
        variants::<RemovalMethod>(),
        variants::<RecoveryMethod>(),
        variants::<ExecutionMethod>(),
        variants::<AssignmentMethod>(),
        variants::<AgentMethod>(),
        variants::<ChatMethod>(),
        variants::<QueueMethod>(),
        variants::<HoldMethod>(),
        variants::<DemoMethod>(),
        variants::<ItemMethod>(),
        variants::<CiMethod>(),
        variants::<NotificationMethod>(),
        variants::<QuestionMethod>(),
        variants::<ApprovalMethod>(),
        variants::<PortMethod>(),
        variants::<SecretMethod>(),
        variants::<ResourceMethod>(),
        variants::<SimulatorMethod>(),
    ]
    .concat()
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(
        variants.difference(&sampled).collect::<Vec<_>>(),
        Vec::<&String>::new()
    );
    assert_eq!(sampled.len(), REQUESTS.len(), "one request per variant");
}

/// The variant names an externally tagged enum declares to serde.
fn variants<'de, T: Deserialize<'de>>() -> &'static [&'static str] {
    let mut variants: &'static [&'static str] = &[];
    let _ = T::deserialize(VariantProbe(&mut variants));
    variants
}

/// A deserializer that records the variants it is asked for, then fails.
struct VariantProbe<'a>(&'a mut &'static [&'static str]);

impl<'de> Deserializer<'de> for VariantProbe<'_> {
    type Error = de::value::Error;

    fn deserialize_any<V: Visitor<'de>>(self, _: V) -> Result<V::Value, Self::Error> {
        Err(de::Error::custom("not an enum"))
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _: &'static str,
        variants: &'static [&'static str],
        _: V,
    ) -> Result<V::Value, Self::Error> {
        *self.0 = variants;
        Err(de::Error::custom("variants recorded"))
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes
        byte_buf option unit unit_struct newtype_struct seq tuple tuple_struct map
        struct identifier ignored_any
    }
}
