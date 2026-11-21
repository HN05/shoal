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
    r#"{"hold_contribution":{"repository":"r1"}}"#,
    r#"{"receive_commits":{"origin":"https://forge.example/team/app","commit":"0123456789abcdef0123456789abcdef01234567","branch":"main"}}"#,
    r#"{"rename_repository":{"repository":{"value":".","path":"/work/app"},"name":"web"}}"#,
    r#"{"local_config_path":{"repository":"app"}}"#,
    r#"{"edit_local_config":{"repository":"app","changes":[{"key":"setup.command","value":null}]}}"#,
    r#"{"remove_repository":{"repository":"app"}}"#,
    r#"{"plan_repository_moves":{"root":"/work","regular_dir":false}}"#,
    r#"{"move_repositories":{"regular_dir":true}}"#,
    r#"{"sync_repository":{"repository":"app"}}"#,
    r#""failing_syncs""#,
    r#"{"list_branches":{"repository":"app"}}"#,
    r#"{"kept_branches":{"repository":"app"}}"#,
    r#"{"delete_kept_branches":{"branches":[{"repository_id":"r1","branch":"fix","head":"abc"}]}}"#,
    r#"{"agent_environments":{"repository_id":"r1"}}"#,
    r#"{"forget_agent_environments":{"repository_id":"r1"}}"#,
    r#"{"keep_repository_agent_environment":{"repository":"app","environment":{"CODEX_HOME":"/codex"}}}"#,
    // Workspaces.
    r#"{"open_branch":{"path":null,"repository":"app","branch":"fix","git_profile":null,"base":"main","parent":null}}"#,
    r#"{"create_workspace":{"path":"/work/fix","repository":"app","name":"fix","base":null,"git_profile":"work","parent":"c1"}}"#,
    r#"{"reopen_workspace":{"workspace":"fix"}}"#,
    r#"{"launch_permissions":{"workspace":"fix","mode":"review","narrowing":{"push":"propose"}}}"#,
    r#"{"held_permissions":{"repository":"r1"}}"#,
    r#"{"create_commit_workspace":{"repository":"app","parent":"fix","start":{"revision":"v1.4.0"},"options":{"skip_setup":true,"disposable":true}}}"#,
    r#"{"open_pull_head":{"repository":"app","url":"https://forge.example/team/app/pulls/8","head":{"number":8,"reference":"refs/pull/8/head","commit":"abc"},"parent":null}}"#,
    r#"{"create_child":{"parent":"fix","name":"part","base":null}}"#,
    r#"{"adopt_workspace":{"repository":"app","path":"/work/fix","copy":false}}"#,
    r#"{"rename_branch":{"workspace":"fix","branch":"fix-2"}}"#,
    r#"{"convert_to_chat":{"workspace":"fix","mode":"plan"}}"#,
    r#""untracked_worktrees""#,
    r#""list_spaces""#,
    r#"{"find_spaces":{"target":{"item":{"kind":"pr","input":"7"}}}}"#,
    r#"{"locate_space":{"space":"3"}}"#,
    r#"{"rename_space":{"space":"3","name":"Login retries"}}"#,
    r#"{"space_places":{"space":"fix"}}"#,
    r#"{"set_base_workspace":{"workspace":"part","base":"fix"}}"#,
    r#"{"inspect_workspace":{"workspace":"fix"}}"#,
    r#"{"verify_workspace":{"workspace":"fix","on_branch":true}}"#,
    r#"{"fast_forward_workspace":{"workspace":"fix","branch":"main"}}"#,
    r#"{"refresh_workspace":{"workspace":"fix"}}"#,
    r#"{"issue_scope":{"space":"fix"}}"#,
    r#"{"revoke_scope":{"space":"fix","token":"t1"}}"#,
    r#"{"space_environment":{"space":"fix"}}"#,
    r#"{"space_status":{"space":"fix"}}"#,
    r#"{"tracked_config":{"space":"7"}}"#,
    r#"{"space_sessions":{"space":"fix"}}"#,
    r#"{"follow_session":{"space":"fix","session":"abc","from":{"p/abc.jsonl":{"offset":12,"size":20}}}}"#,
    r#"{"attach_session":{"space":"fix","viewer":{"session":"shoal-7","again":{"host":"zmx","place":"shoal-7","owned":true},"size":{"rows":40,"columns":120},"term":"xterm-256color","apps":[{"host":"rex","label":"desk"}]}}}"#,
    r#"{"copy_space":{"space":"fix","checkpoint":3,"file":"/work/fix.shoal","carrying":"subtree"}}"#,
    r#"{"diff_base":{"workspace":"fix"}}"#,
    r#"{"conflicts":{"workspace":"fix","target":null}}"#,
    r#"{"diff":{"workspace":"fix","display":{"color":true,"columns":120}}}"#,
    r#"{"preview_merge":{"workspace":"fix","push":true,"method":"squash","message":"Fix"}}"#,
    r#"{"create_pending_worktree":{"workspace":"fix"}}"#,
    r#"{"workspace_hook":{"workspace":"fix","kind":"post_setup"}}"#,
    r#"{"layered_config":{"target":{"space":"fix"}}}"#,
    r#"{"keep_agent_environment":{"workspace":"fix","environment":{}}}"#,
    r#"{"close_owned_place":{"workspace":"fix","place":{"host":"rex","place":"block:1"}}}"#,
    r#"{"place_ownership":{"space":"fix","place":{"host":"herdr","place":"w1:p1"}}}"#,
    r#"{"record_owned_place":{"workspace":"fix","origin":{"host":"rex","place":"block:1","owned":true}}}"#,
    r#""owned_places""#,
    r#"{"hold_acquire":{"space":"fix","name":"review","reason":null}}"#,
    r#"{"hold_release":{"space":"fix","name":"review"}}"#,
    r#"{"hold_list":{"space":"fix"}}"#,
    r#"{"watch_space_events":{"since":4,"follow":true}}"#,
    r#"{"space_log":{"space":"fix","filter":"entries","limit":50}}"#,
    // Removal and restore.
    r#""stop_targets""#,
    r#"{"stop_selection":{"spaces":["3"],"below":true}}"#,
    r#"{"stop_space":{"space":"fix","acknowledge_unproven":false,"launches":"cancel","processes":{"stop":{"requester":42}}}}"#,
    r#"{"check_stop":{"space":"fix"}}"#,
    r#"{"stop_processes":{"space":"fix","selection":"leftovers","requester":42}}"#,
    r#"{"check_removal":{"space":"fix","include_changes":true}}"#,
    r#"{"remove_space":{"space":"fix","unsaved":"all"}}"#,
    r#"{"start_removal":{"space":"fix","unsaved":"all"}}"#,
    r#"{"absorb_space":{"space":"fix","into":"parent","method":"squash","force":true}}"#,
    r#"{"cleanup":{"dry_run":true,"spaces":"all","caches":"none"}}"#,
    r#""removed_spaces""#,
    r#"{"removed_space":{"space":"fix"}}"#,
    r#"{"restore_space":{"space":"fix"}}"#,
    r#"{"drop_restored_sessions":{"space":"fix"}}"#,
    r#"{"export_removed_space":{"space":"fix","file":"/work/fix.shoal","carrying":"subtree"}}"#,
    r#"{"export_moving_space":{"space":"fix","unsaved":"all","destination":{"kind":"ssh","host":"mini","resume":true},"carrying":{"selected":["split"]}}}"#,
    r#"{"end_moved_space":{"space":"fix"}}"#,
    r#"{"abandon_move":{"space":"fix"}}"#,
    r#"{"deliver_move":{"space":"fix"}}"#,
    r#"{"deliver_copy":{"space":"fix","destination":{"machine":"desk","resume":true}}}"#,
    r#"{"settle_move":{"space":"fix"}}"#,
    r#"{"receive_move":{"ticket":"t1","resume":false,"from":"laptop"}}"#,
    r#"{"import_package":{"package":{"file":"/work/fix.shoal"},"repository":"app","sessions":true}}"#,
    r#"{"checkpoint":{"workspace":"fix","name":"before rebase"}}"#,
    r#"{"checkpoints":{"workspace":"fix","after":null}}"#,
    r#"{"saved":{"space":"fix"}}"#,
    r#""saved_summaries""#,
    r#"{"restore_checkpoint":{"workspace":"fix","checkpoint":2}}"#,
    // Assignments and agents.
    r#"{"mark_done":{"space":"fix","cleanup":true,"summary":"No leak"}}"#,
    r#"{"undo_done":{"space":"fix"}}"#,
    r#"{"discard_stopped_agent":{"space":"part","execution":null}}"#,
    r#"{"change_stopped_permissions":{"agents":[{"workspace":"part","execution":"e1"}],"change":{"set":{"push":"propose"},"dropped":["merge"]}}}"#,
    r#"{"mark_ready":{"space":"fix","selection":{"kind":"pr","input":null}}}"#,
    r#"{"clear_ready":{"space":"fix","selection":{"kind":null,"input":null},"draft":true}}"#,
    r#"{"set_agent_state":{"workspace":"fix","state":"idle","session":"s1","turn_end":{"prompt_cache_secs":3600,"final_message":"Which one?"}}}"#,
    r#"{"set_progress":{"space":"fix","change":{"step_created":{"name":"Fix","state":"pending","key":"2"}}}}"#,
    r#"{"progress":{"space":"fix"}}"#,
    r#"{"sleep":{"space":"fix","sleep":"permanent"}}"#,
    r#"{"start_woken_turn":{"workspace":"fix"}}"#,
    r#"{"report_turn_failure":{"workspace":"fix","failure":"rate_limited","turn_end":{"prompt_cache_secs":300}}}"#,
    r#"{"report_subagent":{"space":"fix","change":{"started":{"agent_id":"a1"}}}}"#,
    r#"{"report_subagent_request":{"space":"fix","execution":"e1"}}"#,
    r#"{"report_wake_end":{"space":"fix","ended":{"signalled":{"signal":"SIGTERM"}}}}"#,
    r#"{"report_proxy_retry":{"workspace":"fix","execution":"e1","retry":{"failure":"overloaded","attempt":2,"next_at":1700000000}}}"#,
    r#"{"report_upstream_reset":{"workspace":"fix","execution":"e1","resets_at":1700000000}}"#,
    r#"{"report_input":{"workspace":"fix","execution":"e1","at":1700000000}}"#,
    r#"{"report_typing":{"workspace":"fix","execution":"e1"}}"#,
    r#"{"report_attach":{"space":"fix"}}"#,
    r#"{"report_upstream_limit":{"workspace":"fix","execution":"e1","limit":{"limit":"exhausted","until":1700000000}}}"#,
    r#"{"await_agent_wake":{"workspace":"fix","due":true,"hooked":"e1"}}"#,
    r#"{"space_agents":{"space":"fix"}}"#,
    r#"{"interrupt_turn":{"space":"fix","execution":"e1"}}"#,
    r#"{"restart_agent":{"space":"3","restart":{"wait":true,"change":{"set":{"push":"direct"}}}}}"#,
    r#"{"cancel_restart":{"space":"3"}}"#,
    // Executions.
    r#"{"execute":{"space":"fix","wrapper":{"pid":2,"birth":"b"},"kind":"command","agent":null,"recover":false,"reattach":false,"queue_entry":null}}"#,
    r#"{"reattach":{"execution":"e1","wrapper":{"pid":2,"birth":"b"},"child":null,"group_id":2,"scope_token":"t1","agent":null,"recover":false,"running_ms":0}}"#,
    r#""check_merge""#,
    r#""check_unmerge""#,
    r#""list_agents""#,
    r#"{"session_signalled":{"signal":"terminate"}}"#,
    // Chats.
    r#"{"launch":{"launch":{"target":"app","in":{"space":"3"},"mode":"chat","agent":"claude","task":["Read the build"]}}}"#,
    r#"{"check_launch":{"launch":{"target":"app","base":"main","issue":{"named":"https://forge.example/team/app/issues/42"}}}}"#,
    r#"{"check_creation":{"launch":{"target":"app","branch":"fix","mode":"worker"}}}"#,
    r#"{"launch_modes":{"launch":{"target":"app","machine":"laptop"}}}"#,
    r#"{"labelled_mode":{"launch":{"target":"https://forge.example/team/app/issues/42"}}}"#,
    r#"{"take_parent_repository":{"launch":{"branch":"fix","parent":"7"}}}"#,
    r#"{"receive_session":{"origin":"https://forge.example/team/app","files":[{"agent":"claude","session":"s1","path":"p/s1.jsonl","size":12}]}}"#,
    r#""chat_root""#,
    // The agent queue.
    r#"{"queue_agent":{"workspace":"fix","launch":{"agent":null,"prompt":"Fix the build"}}}"#,
    r#""list_queue""#,
    r#"{"queued_plan":{"entry":"q1"}}"#,
    r#"{"open_start":{"entry":"q1"}}"#,
    r#"{"queue_remove":{"entry":"q1"}}"#,
    r#"{"queue_start":{"entry":"q1"}}"#,
    r#"{"started_launch":{"entry":"q1"}}"#,
    r#"{"queue_hold":{"entry":"q1"}}"#,
    r#"{"queue_claim":{"entry":"q1"}}"#,
    r#"{"queue_attach":{"entry":"q1","space":"fix"}}"#,
    r#"{"queue_fail":{"entry":"q1","error":"no agent"}}"#,
    r#"{"queue_retry":{"entry":"q1","start":"here"}}"#,
    r#"{"set_priority":{"space":"fix","priority":75}}"#,
    r#"{"queue_restore":{"workspace":"fix","execution":"e1","env":{}}}"#,
    // Demos.
    r#"{"list_demos":{"workspace":null}}"#,
    r#"{"demo_progress":{"workspace":"fix","launch":"l1"}}"#,
    r#"{"stop_demos":{"workspace":"fix","name":"web"}}"#,
    r#"{"open_demo_stream":{"workspace":"fix","name":"tui","terminal":{"size":{"rows":24,"columns":80},"term":"xterm-256color"}}}"#,
    // Triggers.
    r#"{"list_triggers":{"context":{"checkout":"r1"}}}"#,
    r#"{"pause_trigger":{"context":null,"name":"triage","paused":true}}"#,
    r#"{"run_trigger":{"context":null,"name":"labelled","request":{"headers":[["X-Forgejo-Event","issues"]],"body":"{}"}}}"#,
    r#""pair_hub""#,
    r#"{"join_hub":{"via":{"command":["ssh","desk","shoal","daemon","connect"]},"name":"laptop"}}"#,
    r#""leave_hub""#,
    r#""update_hub""#,
    r#""hub_members""#,
    r#"{"revoke_hub_member":{"member":"laptop"}}"#,
    r#""hub_directory""#,
    // Issues and PRs.
    r#"{"link_item":{"space":"fix","kind":"issue","item":"1","relation":"related"}}"#,
    r#"{"relink_space":{"space":"fix","links":{"issues":[{"kind":"issue","url":"https://github.com/o/r/issues/1"}],"prs":[],"opened":[],"related":[],"assignees":[{"url":"https://github.com/o/r/issues/1","account":"agent","added":true}]}}}"#,
    r#"{"unlink_items":{"space":"fix","relation":"assigned","kind":null,"item":null}}"#,
    r#"{"set_pr":{"space":"fix","url":"https://forge.example/team/app/pulls/7","clear":false}}"#,
    r#"{"pr_wait":{"workspace":"fix","timeout_secs":60}}"#,
    r#"{"watch_items":{"space":"fix","selection":{"kind":null,"input":"7"},"timeout_secs":null}}"#,
    r#"{"acknowledge_pr_updates":{"space":"fix","deliveries":["d1"],"messages":[3]}}"#,
    r#"{"open_pull":{"workspace":"fix","options":{"title":null,"body":null,"base":null,"draft":true,"labels":[],"reviewers":[],"assignees":[],"milestone":null,"attachments":[]}}}"#,
    r#"{"revert_pull":{"space":"fix","item":"42","options":{"title":null,"body":null,"base":null,"draft":null,"labels":[],"reviewers":[],"assignees":[],"milestone":null,"attachments":[]}}}"#,
    r#"{"merged_pull":{"space":"fix","item":null}}"#,
    r#"{"open_issue":{"space":{"space":"fix"},"issue":{"title":"Bug","body":"","labels":[],"assignees":[],"milestone":null,"attachments":[]},"link":false}}"#,
    r#"{"merge_warning":{"space":{"space":"fix"},"item":"7"}}"#,
    r#"{"merge_path":{"space":"fix"}}"#,
    r#"{"item_action":{"space":{"space":"fix"},"kind":"pr","item":"7","action":{"action":"merge","method":"squash","delete_branch":false},"confirmed":{"warning":{"subject":"PR #7","target":"main","files":["version"]}}}}"#,
    r#"{"list_items":{"repository":"https://forge.example/team/tool","kind":"issue","search":{"state":"open","labels":[],"author":null,"text":"crash","matching":"both","limit":30}}}"#,
    r#"{"labels":{"space":{"space":"fix"},"repository":"team/tool"}}"#,
    r#"{"reviewers":{"checkout":"repo","item":"12"}}"#,
    r#"{"capabilities":{"space":{"space":"talk"},"repository":"other/repo"}}"#,
    r#"{"reactions":{"space":{"space":"talk","repository":"r1"}}}"#,
    r#"{"review_thread":{"space":{"space":"fix"},"item":null,"comment":31}}"#,
    r#"{"number_kind":{"space":{"space":"fix"},"number":12}}"#,
    r#"{"issue_details":{"checkout":"repo","item":"12"}}"#,
    r#"{"issue_blockers":{"checkout":"repo","item":"https://forge.example/o/r/issues/12"}}"#,
    r#"{"pull_request":{"space":{"space":"talk"},"item":"7"}}"#,
    r#"{"view_items":{"repository":"https://forge.example/team/tool","selection":{"kind":"pr","input":"12"},"comments":true}}"#,
    r#"{"forge_api":{"space":{"space":"fix"},"request":{"method":"GET","path":"repos/{owner}/{repo}","body":null}}}"#,
    r#"{"forge_download":{"space":{"space":"fix"},"url":"https://forge.example/attachments/a1"}}"#,
    r#"{"select_items":{"checkout":"repo","selection":{"kind":null,"input":"12"},"elsewhere":true}}"#,
    // CI.
    r#"{"check_log":{"space":{"space":"fix"},"selection":{"kind":"pr","input":null},"check":"rust"}}"#,
    r#"{"ci_action":{"space":{"space":"fix"},"item":null,"action":"rerun","checks":["rust"]}}"#,
    r#"{"ci_run_action":{"space":{"space":"fix"},"run":"12","action":"cancel","failed":false}}"#,
    r#"{"view_ci_run":{"space":{"space":"fix"},"run":"12"}}"#,
    r#"{"ci_run_jobs":{"space":{"space":"fix"},"run":"12"}}"#,
    r#"{"ci_run_job_log":{"space":{"space":"fix"},"run":"12","job":{"name":"rust"}}}"#,
    r#"{"list_ci_runs":{"repository":"https://forge.example/team/tool","filter":{"branch":null,"workflow":"ci.yml","limit":10}}}"#,
    r#"{"start_ci_run":{"space":{"space":"fix"},"workflow":null,"branch":null,"inputs":{"level":"high"}}}"#,
    r#"{"approve_ci_runs":{"space":{"space":"fix"},"run":null,"pr":"7"}}"#,
    // Branches on the forge.
    r#"{"list_forge_branches":{"space":{"space":"fix"},"merged":false}}"#,
    r#"{"delete_forge_branches":{"checkout":"repo","repository":"team/tool","branches":["old-fix"],"force":true}}"#,
    // Reconciliation.
    r#"{"reconcile":{"workspace":null,"options":{"repair":true,"stop":false,"acknowledge_stopped":false,"reclaim":false}}}"#,
    // Alerts.
    r#"{"list_alerts":{"to":{"space":{"space":"fix"}},"unread_only":false,"limit":50}}"#,
    r#"{"mark_alerts_read":{"ids":[1,2]}}"#,
    r#""clear_alerts""#,
    r#"{"watch_alerts":{"keep_unread":false,"after":3}}"#,
    r#"{"send_alert":{"to":{"space":{"space":"part"}},"message":"rebase","attachments":[{"path":"/work/log.txt","title":"Crash log"},{"path":"/work/shot.png"}]}}"#,
    r#"{"withdraw_alert":{"id":4}}"#,
    r#"{"git_credential":{"host":"git.example.com"}}"#,
    r#"{"agent_updates":{"space":"fix"}}"#,
    r#"{"wait_agent_updates":{"space":"app-chat"}}"#,
    r#"{"updates_waiting":{"space":"fix"}}"#,
    r#"{"agent_session":{"space":"app-chat"}}"#,
    // Threads.
    r#"{"open_thread":{"name":"Which database?","kind":"question","to":"part","from":"planning"}}"#,
    r#"{"post":{"thread":4,"post":{"text":"Which?","choices":["A","B"],"recommendation":"A","reason":"Simpler"},"changes":[12,14]}}"#,
    r#"{"post_in_chat":{"space":"fix","thread":6}}"#,
    r#"{"alert_in_chat":{"to":{"space":{"space":"part"}},"attachments":[{"path":"/fix/log.txt"}]}}"#,
    r#"{"replied_in_chat":{"space":"fix","prompt":"Use SQLite"}}"#,
    r#"{"forward_thread":{"thread":6,"to":"part","note":"You built the cache"}}"#,
    r#"{"resolve_thread":{"thread":6,"outcome":"duplicate","of":{"space_number":2,"item":1,"number":"2.1"}}}"#,
    r#"{"list_threads":{"space":"fix","state":"open","for_user":true}}"#,
    r#"{"show_thread":{"thread":{"space_number":2,"item":6,"number":"2.6"}}}"#,
    r#"{"link_thread":{"space":"fix","thread":{"space_number":2,"item":6,"number":"2.6"}}}"#,
    r#"{"unlink_thread":{"space":"fix","thread":{"space_number":2,"item":6,"number":"2.6"}}}"#,
    r#"{"await_post":{"space":"fix","item":6,"after":12}}"#,
    r#"{"switch_thread":{"space":"fix","thread":4}}"#,
    r#"{"current_thread":{"space":"fix"}}"#,
    r#"{"add_changes":{"thread":4,"changes":[12,{"space_number":3,"item":2,"number":"3.2"}]}}"#,
    r#"{"remove_changes":{"thread":4,"space":"fix","changes":[12]}}"#,
    // Changes.
    r#"{"record_change":{"change":{"kind":"edited","path":"/work/fix/lib.rs","hunks":[{"before":3,"lines":{"start":3,"end":4},"removed":["a"],"added":["b"]}]}}}"#,
    r#"{"list_changes":{"filter":{"thread":4,"since":2}}}"#,
    r#"{"change_diffs":{"filter":{"space":"fix"}}}"#,
    r#"{"claim_changes":{"changes":[12],"thread":4}}"#,
    r#"{"unclaim_changes":{"space":"fix","changes":[12]}}"#,
    r#"{"report_edits":{"paths":["/work/fix/lib.rs"]}}"#,
    // Approvals.
    r#"{"list_approvals":{"space":null}}"#,
    r#"{"show_approval":{"number":{"space_number":2,"item":7,"number":"2.7"}}}"#,
    r#"{"decide_approval":{"number":{"space_number":2,"item":7,"number":"2.7"},"approve":true,"details":{"reason":"later"}}}"#,
    r#"{"escalate_approval":{"space":"fix","number":{"space_number":2,"item":7,"number":"2.7"}}}"#,
    r#"{"withdraw_approval":{"space":"fix","number":{"space_number":2,"item":7,"number":"2.7"}}}"#,
    r#"{"branch_push":{"workspace":"fix"}}"#,
    r#"{"request_push":{"workspace":"fix","push":{"remote":"origin","destination":"refs/heads/fix","commit":"abc"}}}"#,
    r#"{"finish_push":{"key":"key","failure":"rejected"}}"#,
    r#"{"authorize_action":{"key":"daemon","action":"shoal daemon restart"}}"#,
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
    r#"{"container_catalog":{"workspace":"fix"}}"#,
    r#"{"container_overview":{"workspace":null}}"#,
    r#"{"container_acquire":{"workspace":"fix","reason":"database"}}"#,
    r#"{"container_release":{"workspace":"fix"}}"#,
    r#"{"container_set_up":{"workspace":"fix","for_agent":true}}"#,
    r#"{"container_agent_start":{"workspace":"fix"}}"#,
    r#"{"sim_acquire":{"workspace":"fix","request":{"request_id":"u1","clean":false,"name":"default","profile":null,"device":null,"runtime":null,"reason":null,"queue":false}}}"#,
    r#"{"sim_release":{"workspace":"fix","name":"default"}}"#,
    r#"{"sim_history":{"workspace":null,"limit":20,"before":null}}"#,
    // Subscriptions.
    r#"{"subscribe":{"topics":["spaces","threads"]}}"#,
    // Operations that create a workspace.
    r#"{"start_workspace":{"repository":"app","name":"fix","issue":"https://forge.example/team/app/issues/42","agent":{"agent":"claude","prompt":null}}}"#,
    r#"{"start_creation":{"launch":{"target":"app","branch":"fix","parent":"3"},"below":"4"}}"#,
    r#"{"start_child":{"parent":"fix","name":"part","base":"HEAD"}}"#,
    r#"{"convert_to_workspace":{"chat":"app-chat","branch":"fix","issue":"https://forge.example/team/app/issues/42"}}"#,
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

/// Where a started workspace starts sits beside the request's other fields,
/// so an app's request for a new branch keeps its JSON.
#[test]
fn a_workspace_start_keeps_its_json() {
    for wire in [
        r#"{"start_workspace":{"repository":"app","branch":"origin/fix","path":"/work/fix","git_profile":"work"}}"#,
        r#"{"start_workspace":{"repository":"app","typed":"fix/login","base":"main"}}"#,
        r#"{"start_workspace":{"repository":"app","pull_head":{"url":"https://forge.example/team/app/pulls/7","head":{"number":7,"reference":"refs/pull/7/head","commit":"abc"}},"parent":"c1","options":{"skip_setup":true,"disposable":false},"pr":"https://forge.example/team/app/pulls/7","env":{"PATH":"/bin"}}}"#,
    ] {
        let method: Method = serde_json::from_str(wire).unwrap();
        assert_eq!(serde_json::to_string(&method).unwrap(), wire);
    }
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
        variants::<LaunchMethod>(),
        variants::<QueueMethod>(),
        variants::<HoldMethod>(),
        variants::<DemoMethod>(),
        variants::<ItemMethod>(),
        variants::<CiMethod>(),
        variants::<BranchMethod>(),
        variants::<AlertMethod>(),
        variants::<ThreadMethod>(),
        variants::<ChangeMethod>(),
        variants::<ApprovalMethod>(),
        variants::<PortMethod>(),
        variants::<SecretMethod>(),
        variants::<ResourceMethod>(),
        variants::<SimulatorMethod>(),
        variants::<ContainerMethod>(),
        variants::<SubscriptionMethod>(),
        variants::<TriggerMethod>(),
        variants::<HubMethod>(),
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
