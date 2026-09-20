//! Native CMD input comes from the marked grid, including history recall.
use super::*;
use nebula_terminal::event_loop::StreamProcessor;
use startup_tests::open;

fn prompt_bytes(prompt: &str, input: &str) -> Vec<u8> {
    format!(
        "\x1b]133;A\x07{prompt}\x1b]133;B\x07\x1b]1337;SetUserVar=pebrel_cmd_prompt=MQ==\x07{input}"
    )
    .into_bytes()
}

fn root_process() -> Vec<crate::process_tree::ProcessEntry> {
    vec![crate::process_tree::ProcessEntry {
        pid: 1,
        parent_pid: 0,
        depth: 0,
        executable: "cmd.exe".into(),
    }]
}

#[gpui::test]
fn custom_native_prompts_capture_recalled_input_and_keep_submission_epochs(
    cx: &mut gpui::TestAppContext,
) {
    for prompt in ["[C:\\work] ", "[C:\\work]\r\n>", "", "工作目录 :: ", &"x".repeat(79)] {
        let (view, window, _) = open(cx);
        view.update(window, |view, cx| {
            let (session, _input, mut events, proxy) = session::test_session_with_events();
            view.session = Some(session);
            view.suggest.suggest_env = crate::display::SuggestEnv::Local;
            let mut stream = StreamProcessor::default();
            stream.feed(
                &mut view.session.as_ref().unwrap().term.lock(),
                &proxy,
                &prompt_bytes(prompt, "pause"),
            );
            view.suggest.line_buf.clear();
            view.commit_line(cx);
            assert_eq!(view.suggest.last_committed, "pause", "prompt {prompt:?}");
            assert!(view.command_running);
            view.write_input(b"\r".to_vec(), cx);
            while let Ok(event) = events.try_recv() {
                view.process_event(event, cx);
            }
            view.apply_prompt_process_probe(
                view.command_started,
                view.prompt_input_epoch,
                Ok(root_process()),
                cx,
            );
            assert!(view.command_running, "queued old prompt must not finish pause");

            let mut next = b"\r\n".to_vec();
            next.extend(prompt_bytes(prompt, ""));
            stream.feed(&mut view.session.as_ref().unwrap().term.lock(), &proxy, &next);
            while let Ok(event) = events.try_recv() {
                view.process_event(event, cx);
            }
            view.apply_prompt_process_probe(
                view.command_started,
                view.prompt_input_epoch,
                Ok(root_process()),
                cx,
            );
            assert!(!view.command_running, "fresh prompt ends the builtin wait");
        });
    }
}

#[gpui::test]
fn custom_native_prompts_handle_empty_submit_paste_and_runtime_without_fake_history(
    cx: &mut gpui::TestAppContext,
) {
    for prompt in ["", "[C:\\work] ", "[C:\\work]\r\n>"] {
        for action in ["empty", "paste", "runtime"] {
            let (view, window, _) = open(cx);
            view.update(window, |view, cx| {
                let (session, _input, _events, proxy) = session::test_session_with_events();
                view.session = Some(session);
                view.suggest.suggest_env = crate::display::SuggestEnv::Local;
                StreamProcessor::default().feed(
                    &mut view.session.as_ref().unwrap().term.lock(),
                    &proxy,
                    &prompt_bytes(prompt, ""),
                );
                assert!(crate::display::nebula_shell_ready_from_raw_grid(
                    &view.session.as_ref().unwrap().term.lock(),
                    &view.suggest.suggest_env,
                ));
                match action {
                    "empty" => view.commit_line(cx),
                    "paste" => view.paste_now_impl("pause\r\n", false, cx),
                    _ => {
                        view.runtime_prompt("pause".into(), true, cx).unwrap();
                    },
                }
                assert_eq!(view.command_running, action != "empty", "{prompt:?} {action}");
                assert!(view.suggest.last_committed.is_empty(), "unconfirmed input is not history");
                if action != "empty" {
                    assert!(view.suggest.pending_command_prompt.is_some());
                }
            });
        }
    }
}
