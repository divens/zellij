use crate::os_input_output::ClientOsApi;
use crate::stdin_ansi_parser::StdinAnsiParser;
use crate::InputInstruction;
use std::sync::{Arc, Mutex};
use zellij_utils::channels::SenderWithContext;

#[cfg(not(windows))]
use crate::keyboard_parser::KittyKeyboardParser;
#[cfg(not(windows))]
use termwiz::input::{InputEvent, InputParser};

#[cfg(not(windows))]
fn send_done_parsing_after_query_timeout(
    send_input_instructions: SenderWithContext<InputInstruction>,
    query_duration: u64,
) {
    std::thread::spawn({
        move || {
            std::thread::sleep(std::time::Duration::from_millis(query_duration));
            send_input_instructions
                .send(InputInstruction::DoneParsing)
                .unwrap();
        }
    });
}

pub(crate) fn stdin_loop(
    mut os_input: Box<dyn ClientOsApi>,
    send_input_instructions: SenderWithContext<InputInstruction>,
    stdin_ansi_parser: Arc<Mutex<StdinAnsiParser>>,
    explicitly_disable_kitty_keyboard_protocol: bool,
) {
    {
        // on startup we send a query to the terminal emulator for stuff like the pixel size and colors
        // we get a response through STDIN, so it makes sense to do this here
        let mut stdin_ansi_parser = stdin_ansi_parser.lock().unwrap();
        match stdin_ansi_parser.read_cache() {
            Some(events) => {
                let _ =
                    send_input_instructions.send(InputInstruction::AnsiStdinInstructions(events));
                let _ = send_input_instructions
                    .send(InputInstruction::DoneParsing)
                    .unwrap();
            },
            None => {
                // On Windows, the ANSI terminal query mechanism (writing escape
                // sequences to stdout and reading responses from stdin) does not
                // work reliably — Windows Terminal may not deliver responses
                // through the console input buffer in a way that fill_buf() can
                // read, causing the startup to hang.  Skip it for now; pixel
                // dimensions and color registers are nice-to-have, not critical.
                #[cfg(not(windows))]
                {
                    send_input_instructions
                        .send(InputInstruction::StartedParsing)
                        .unwrap();
                    let terminal_emulator_query_string =
                        stdin_ansi_parser.terminal_emulator_query_string();
                    let _ = os_input
                        .get_stdout_writer()
                        .write(terminal_emulator_query_string.as_bytes())
                        .unwrap();
                    let query_duration = stdin_ansi_parser.startup_query_duration();
                    send_done_parsing_after_query_timeout(
                        send_input_instructions.clone(),
                        query_duration,
                    );
                }
                #[cfg(windows)]
                {
                    let _ = send_input_instructions
                        .send(InputInstruction::DoneParsing);
                }
            },
        }
    }
    // On Windows, always use crossterm's event::read() which reads native
    // INPUT_RECORDs via ReadConsoleInput. This works both on the native console
    // (cmd.exe, PowerShell, Windows Terminal) and inside terminal emulators like
    // Alacritty, because conpty translates VT sequences into INPUT_RECORDs before
    // the child process sees them. Crossterm properly extracts ALT from the
    // dwControlKeyState modifier flags.
    #[cfg(windows)]
    {
        use crossterm::event::{self, Event, KeyEventKind};
        use zellij_utils::input::cast_crossterm_key;

        let _ = (stdin_ansi_parser, explicitly_disable_kitty_keyboard_protocol);

        loop {
            let evt = event::read();
            match &evt {
                Ok(e) => log::info!("crossterm event: {:?}", e),
                Err(e) => log::error!("crossterm error: {:?}", e),
            }
            match evt {
                Ok(Event::Key(key_event)) => {
                    if key_event.kind != KeyEventKind::Press {
                        continue;
                    }
                    if let Some((key, bytes)) = cast_crossterm_key(key_event) {
                        if send_input_instructions
                            .send(InputInstruction::KeyWithModifierEvent(key, bytes, false))
                            .is_err()
                        {
                            break;
                        }
                    }
                },
                Ok(Event::Paste(text)) => {
                    let raw_bytes = text.as_bytes().to_vec();
                    let paste_event = termwiz::input::InputEvent::Paste(text);
                    if send_input_instructions
                        .send(InputInstruction::KeyEvent(paste_event, raw_bytes))
                        .is_err()
                    {
                        break;
                    }
                },
                Ok(Event::Resize(..)) => {
                    // Handled by the signal handler thread
                },
                Ok(_) => {},
                Err(e) => {
                    log::error!("Failed to read crossterm event: {}", e);
                    let _ = send_input_instructions.send(InputInstruction::Exit);
                    break;
                },
            }
        }
    }

    #[cfg(not(windows))]
    {
        let mut input_parser = InputParser::new();
        let mut current_buffer = vec![];
        let mut ansi_stdin_events = vec![];
        loop {
            match os_input.read_from_stdin() {
                Ok(buf) => {
                    {
                        // here we check if we need to parse specialized ANSI instructions sent over STDIN
                        // this happens either on startup (see above) or on SIGWINCH
                        //
                        // if we need to parse them, we do so with an internal timeout - anything else we
                        // receive on STDIN during that timeout is unceremoniously dropped
                        let mut stdin_ansi_parser = stdin_ansi_parser.lock().unwrap();
                        if stdin_ansi_parser.should_parse() {
                            let events = stdin_ansi_parser.parse(buf);
                            if !events.is_empty() {
                                ansi_stdin_events.append(&mut events.clone());
                                let _ = send_input_instructions
                                    .send(InputInstruction::AnsiStdinInstructions(events));
                            }
                            continue;
                        }
                    }
                    if !ansi_stdin_events.is_empty() {
                        stdin_ansi_parser
                            .lock()
                            .unwrap()
                            .write_cache(ansi_stdin_events.drain(..).collect());
                    }
                    current_buffer.append(&mut buf.to_vec());

                    if !explicitly_disable_kitty_keyboard_protocol {
                        // first we try to parse with the KittyKeyboardParser
                        // if we fail, we try to parse normally
                        match KittyKeyboardParser::new().parse(&buf) {
                            Some(key_with_modifier) => {
                                send_input_instructions
                                    .send(InputInstruction::KeyWithModifierEvent(
                                        key_with_modifier,
                                        current_buffer.drain(..).collect(),
                                        true,
                                    ))
                                    .unwrap();
                                continue;
                            },
                            None => {},
                        }
                    }

                    let maybe_more = false; // read_from_stdin should (hopefully) always empty the STDIN buffer completely
                    let mut events = vec![];
                    input_parser.parse(
                        &buf,
                        |input_event: InputEvent| {
                            events.push(input_event);
                        },
                        maybe_more,
                    );

                    for input_event in events.into_iter() {
                        send_input_instructions
                            .send(InputInstruction::KeyEvent(
                                input_event,
                                current_buffer.drain(..).collect(),
                            ))
                            .unwrap();
                    }
                },
                Err(e) => {
                    if e == "Session ended" {
                        log::debug!("Switched sessions, signing this thread off...");
                    } else {
                        log::error!("Failed to read from STDIN: {}", e);
                    }
                    let _ = send_input_instructions.send(InputInstruction::Exit);
                    break;
                },
            }
        }
    }
}
