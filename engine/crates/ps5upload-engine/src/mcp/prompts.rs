//! Ready-made starting points a client can offer its user. Each names the tools to use, in the
//! order that works, and what must not be done without asking.

use serde_json::{json, Value};

use super::jsonrpc::RpcError;

const CONSOLE_ARGUMENT: &str = "The PS5's address, like 192.168.1.100. Left out, consoles_list \
                                helps to find it.";

/// The answer to `prompts/list`.
pub(crate) fn list() -> Value {
    json!({
        "prompts": [
            {
                "name": "play_session",
                "title": "Play through Remote Play",
                "description": "Watch a running Remote Play session and press buttons, one look \
                                at a time, toward a goal.",
                "arguments": [
                    {
                        "name": "goal",
                        "description": "What to do in the game, in the user's words.",
                        "required": true,
                    },
                    { "name": "console", "description": CONSOLE_ARGUMENT, "required": false },
                ],
            },
            {
                "name": "install_game",
                "title": "Install a game",
                "description": "Check the console, install a package from where it is, follow it \
                                to the end, and say what happened.",
                "arguments": [
                    {
                        "name": "source",
                        "description": "The package: a path on the console or on the engine's \
                                        computer, or a link.",
                        "required": true,
                    },
                    { "name": "console", "description": CONSOLE_ARGUMENT, "required": false },
                ],
            },
            {
                "name": "diagnose_connection",
                "title": "Diagnose a console connection",
                "description": "Find out why a console does not answer, or answers badly, and say \
                                what would fix it. Changes nothing.",
                "arguments": [
                    { "name": "console", "description": CONSOLE_ARGUMENT, "required": false },
                ],
            },
            {
                "name": "free_up_space",
                "title": "Free up console space",
                "description": "See what takes the space on a console and suggest what could go. \
                                Deletes only what the user confirms, one thing at a time.",
                "arguments": [
                    { "name": "console", "description": CONSOLE_ARGUMENT, "required": false },
                    {
                        "name": "target_gb",
                        "description": "How many GB the user wants free.",
                        "required": false,
                    },
                ],
            },
        ],
    })
}

/// The answer to `prompts/get`.
pub(crate) fn get(params: &Value) -> Result<Value, RpcError> {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return Err(RpcError::invalid_params("prompts/get needs a \"name\"."));
    };
    let argument = |key: &str| {
        params
            .get("arguments")
            .and_then(|arguments| arguments.get(key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
    };
    let console = match argument("console") {
        Some(console) => format!("the console at {console}"),
        None => "the console the user means (consoles_list shows the known ones; if it is not \
                 clear which, ask)"
            .to_string(),
    };
    let (description, text) = match name {
        "play_session" => {
            let Some(goal) = argument("goal") else {
                return Err(RpcError::invalid_params(
                    "The play_session prompt needs a \"goal\".",
                ));
            };
            ("Play through Remote Play", play_session(&console, goal))
        }
        "install_game" => {
            let Some(source) = argument("source") else {
                return Err(RpcError::invalid_params(
                    "The install_game prompt needs a \"source\".",
                ));
            };
            ("Install a game", install_game(&console, source))
        }
        "diagnose_connection" => (
            "Diagnose a console connection",
            diagnose_connection(&console),
        ),
        "free_up_space" => (
            "Free up console space",
            free_up_space(&console, argument("target_gb")),
        ),
        other => {
            return Err(RpcError::invalid_params(format!("Unknown prompt: {other}")));
        }
    };
    Ok(json!({
        "description": description,
        "messages": [{ "role": "user", "content": { "type": "text", "text": text } }],
    }))
}

fn play_session(console: &str, goal: &str) -> String {
    format!(
        "Play through the running Remote Play session on {console}. The goal: {goal}\n\n\
         1. Call rp_session. If nothing streams, call rp_start for that console: asking you to \
         play is asking for a stream. If it says `no_credentials`, ask me to pair the console in \
         the PS5Upload app. If it says the console wants a passcode, ask me for it and send it \
         with rp_passcode.\n\
         2. Call rp_frame to see the screen. If `frame_age_ms` is large, the stream stalled or the \
         app window is hidden: say so.\n\
         3. Decide one small step toward the goal and send it with rp_input, naming the `session` \
         from step 1: short holds, 100 to 200 ms for a menu.\n\
         4. Call rp_frame with `after` set to the last `frame_id` to see what happened. Only then \
         decide the next step: a sent input does not mean the game reacted.\n\
         5. If a call fails, look at a new frame before you send anything again. If the session \
         changes (`stale_session`), go back to step 1.\n\
         6. When the goal is reached or you are stuck, call rp_input_release and tell me where \
         things stand. If you started the stream, ask me before you stop it.\n\n\
         Do not buy anything, delete saves, or change system settings on screen unless I say so."
    )
}

fn install_game(console: &str, source: &str) -> String {
    format!(
        "Install this package on {console}: {source}\n\n\
         1. Call consoles_status and consoles_readiness. If the console is not ready, say why and \
         stop: do not try anyway.\n\
         2. Call install_pkg with the `source_type` that fits: console_path for a path on the \
         console, host_file for a path on the computer the engine runs on, url for a link, remote \
         for a saved server (sources_list).\n\
         3. Follow it with install_wait, calling it again while `finished` is false. If it says \
         another install runs, follow that one instead of starting over.\n\
         4. Say how it ended, in plain words, with its `verdict` and `hint`. If it failed, do not \
         retry on your own: say what you would change, and ask.\n\n\
         Never set allow_destructive_reinstall or delete_source_copy_after unless I say so. Do not \
         launch the game afterwards unless I ask."
    )
}

fn diagnose_connection(console: &str) -> String {
    format!(
        "Find out why {console} does not answer as it should, and tell me what to do about it. \
         Only look: change nothing.\n\n\
         1. Call consoles_status. If it answers, the helper is fine: call consoles_readiness and \
         hw_info to look further.\n\
         2. If it fails, read `reason`. console_rest_mode: the console sleeps. not_paired: pair it \
         in the app. helper_not_ava1: the helper is missing or old; send it from the app's \
         Connection screen. ava1_unreachable or a timeout: go on with step 3.\n\
         3. Check the network with api_request: GET /api/ps5/port-check with query `ip` (the \
         console's address) and `port` 9021 (the payload loader) and 9120 (the helper). Then GET \
         /api/net/doctor with query `addr`, for this computer's link toward the console.\n\
         4. Say in plain words what you found and the one next step that would help most. Wake or \
         reboot the console only if I ask."
    )
}

fn free_up_space(console: &str, target_gb: Option<&str>) -> String {
    let target = match target_gb {
        Some(target) => format!("I want {target} GB free."),
        None => "Tell me how much could be freed.".to_string(),
    };
    format!(
        "Help me free up space on {console}. {target}\n\n\
         1. Call hw_storage for the internal storage, and files_volumes for every drive.\n\
         2. Call games_installed with the console, then find what takes the space: \
         disk_usage_start walks a folder, like /data; disk_usage_status follows it until `state` \
         says it ended, and `tree` then holds the sizes. cleanup_scan finds leftovers the console \
         no longer needs.\n\
         3. Show me a short list, largest first: what it is, how big, and what is lost if it goes. \
         Leave out anything you cannot explain. Never suggest system folders.\n\
         4. Delete only what I confirm by name, one thing at a time, with files_delete or \
         cleanup_apply: it cannot be undone. If it answers with an op_id, follow it with \
         files_op_status.\n\
         5. Call hw_storage again and tell me what was gained."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(answer: &Value) -> &str {
        answer["messages"][0]["content"]["text"].as_str().unwrap()
    }

    #[test]
    fn each_listed_prompt_answers() {
        let prompts = list();
        for prompt in prompts["prompts"].as_array().unwrap() {
            let name = prompt["name"].as_str().unwrap();
            let answer = get(&json!({
                "name": name,
                "arguments": {"goal": "open the menu", "source": "/data/a.pkg"},
            }))
            .unwrap();
            assert_eq!(answer["messages"][0]["role"], "user", "{name}");
            assert!(text_of(&answer).len() > 100, "{name}");
        }
    }

    #[test]
    fn arguments_reach_the_text() {
        let answer = get(&json!({
            "name": "play_session",
            "arguments": {"goal": "open the menu", "console": "1.2.3.4"},
        }))
        .unwrap();
        let text = text_of(&answer);
        assert!(
            text.contains("open the menu") && text.contains("1.2.3.4"),
            "{text}"
        );

        let answer = get(&json!({
            "name": "free_up_space",
            "arguments": {"target_gb": "100"},
        }))
        .unwrap();
        let text = text_of(&answer);
        assert!(
            text.contains("100 GB") && text.contains("consoles_list"),
            "{text}"
        );
    }

    #[test]
    fn a_missing_required_argument_or_unknown_prompt_is_invalid_params() {
        let no_goal = get(&json!({"name": "play_session"})).unwrap_err();
        assert_eq!(no_goal.code, crate::mcp::jsonrpc::INVALID_PARAMS);
        let unknown = get(&json!({"name": "nope"})).unwrap_err();
        assert_eq!(unknown.code, crate::mcp::jsonrpc::INVALID_PARAMS);
        assert!(unknown.message.contains("nope"));
        assert!(get(&json!({})).is_err());
    }

    #[test]
    fn every_tool_a_prompt_names_exists() {
        for text in [
            play_session("c", "g"),
            install_game("c", "s"),
            diagnose_connection("c"),
            free_up_space("c", None),
        ] {
            for word in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
                let looks_like_a_tool = word.contains('_')
                    && word.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
                    && [
                        "rp_",
                        "consoles_",
                        "hw_",
                        "games_",
                        "files_",
                        "api_",
                        "jobs_",
                        "install_",
                        "disk_",
                        "cleanup_",
                        "sources_",
                        "payloads_",
                        "settings_",
                    ]
                    .iter()
                    .any(|family| word.starts_with(family));
                if looks_like_a_tool {
                    assert!(
                        crate::mcp::tools::find(word).is_some(),
                        "a prompt names {word}, which is no tool"
                    );
                }
            }
        }
    }
}
