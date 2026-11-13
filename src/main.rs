//! millgui skeleton: a LinuxCNC DISPLAY program that owns a HAL component,
//! reads two pins netted by `millgui_postgui.hal`, and writes two of its own.

mod hal;

use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::time::Duration;

slint::slint! {
    import { Button } from "std-widgets.slint";

    export component MainWindow inherits Window {
        title: "millgui";
        preferred-width: 800px;
        preferred-height: 480px;
        background: #1d2125;

        in property <bool> machine-on;
        in property <bool> estop;
        in property <bool> output-on;
        in property <int> heartbeat;
        in property <string> status;
        callback toggle-output();
        callback quit();

        VerticalLayout {
            padding: 24px;
            spacing: 16px;
            Text { text: "millgui skeleton"; font-size: 28px; color: #e8e8e8; }
            Text { text: root.status; font-size: 16px; color: #9aa4ad; wrap: word-wrap; }

            HorizontalLayout {
                spacing: 16px;
                Rectangle {
                    height: 80px;
                    border-radius: 8px;
                    background: root.estop ? #b3261e : #2e7d32;
                    Text { text: root.estop ? "E-STOP ACTIVE" : "E-STOP CLEAR"; font-size: 22px; color: white; }
                }
                Rectangle {
                    height: 80px;
                    border-radius: 8px;
                    background: root.machine-on ? #2e7d32 : #444b52;
                    Text { text: root.machine-on ? "MACHINE ON" : "MACHINE OFF"; font-size: 22px; color: white; }
                }
            }

            HorizontalLayout {
                spacing: 16px;
                Button {
                    height: 80px;
                    text: root.output-on ? "millgui.output = 1" : "millgui.output = 0";
                    clicked => { root.toggle-output(); }
                }
                Text {
                    vertical-alignment: center;
                    text: "heartbeat " + root.heartbeat;
                    font-size: 18px;
                    color: #9aa4ad;
                }
            }

            Rectangle { }  // spacer
            HorizontalLayout {
                alignment: end;
                Button { text: "Exit"; height: 60px; width: 160px; clicked => { root.quit(); } }
            }
        }
    }
}

struct Pins {
    machine_on: hal::BitPin,
    estop: hal::BitPin,
    output: hal::BitPin,
    heartbeat: hal::S32Pin,
}

fn ini_path() -> Option<PathBuf> {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "-ini") {
        return args.get(i + 1).map(PathBuf::from);
    }
    std::env::var_os("INI_FILE_NAME").map(PathBuf::from)
}

/// Run the [MILLGUI]POSTGUI_HALFILE after our pins exist, the same way the
/// stock GUIs run [HAL]POSTGUI_HALFILE. We use our own key because the included
/// mymill.ini already points [HAL]POSTGUI_HALFILE at Probe Basic's file.
fn run_postgui(ini: &PathBuf) -> Result<String, String> {
    let out = Command::new("inivar")
        .args(["-ini"]).arg(ini)
        .args(["-var", "POSTGUI_HALFILE", "-sec", "MILLGUI"])
        .output()
        .map_err(|e| format!("inivar: {e}"))?;
    let file = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if file.is_empty() {
        return Ok("no [MILLGUI]POSTGUI_HALFILE".into());
    }
    let status = Command::new("halcmd")
        .args(["-i"]).arg(ini)
        .args(["-f", &file])
        .status()
        .map_err(|e| format!("halcmd: {e}"))?;
    if !status.success() {
        return Err(format!("halcmd -f {file} failed: {status}"));
    }
    Ok(format!("ran {file}"))
}

fn setup_hal() -> Result<(hal::Component, Pins), String> {
    let comp = hal::Component::new("millgui")?;
    let pins = Pins {
        machine_on: comp.bit("machine-is-on", hal::Dir::In)?,
        estop: comp.bit("estop-is-activated", hal::Dir::In)?,
        output: comp.bit("output", hal::Dir::Out)?,
        heartbeat: comp.s32("heartbeat", hal::Dir::Out)?,
    };
    comp.ready()?;
    Ok((comp, pins))
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = MainWindow::new()?;

    // Keep the component alive for the whole event loop; Drop calls hal_exit.
    let hal = setup_hal();
    let (_comp, pins) = match hal {
        Ok((c, p)) => {
            let msg = match ini_path() {
                Some(ini) => match run_postgui(&ini) {
                    Ok(m) => format!("HAL ready; {m}; ini {}", ini.display()),
                    Err(e) => format!("HAL ready; postgui error: {e}"),
                },
                None => "HAL ready; no -ini given".to_string(),
            };
            eprintln!("millgui: {msg}");
            ui.set_status(msg.into());
            (Some(c), Some(Rc::new(p)))
        }
        Err(e) => {
            eprintln!("millgui: HAL unavailable: {e}");
            ui.set_status(format!("HAL unavailable: {e}").into());
            (None, None)
        }
    };

    if let Some(p) = pins.clone() {
        let weak = ui.as_weak();
        ui.on_toggle_output(move || {
            p.output.set(!p.output.get());
            if let Some(ui) = weak.upgrade() {
                ui.set_output_on(p.output.get());
            }
        });
    }
    ui.on_quit(|| {
        let _ = slint::quit_event_loop();
    });

    // LinuxCNC stops the display with SIGTERM; leave the loop so hal_exit runs.
    let _ = ctrlc::set_handler(|| {
        let _ = slint::invoke_from_event_loop(|| {
            let _ = slint::quit_event_loop();
        });
    });

    let timer = slint::Timer::default();
    if let Some(p) = pins {
        let weak = ui.as_weak();
        let mut beat = 0i32;
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(50), move || {
            beat = beat.wrapping_add(1);
            p.heartbeat.set(beat);
            if let Some(ui) = weak.upgrade() {
                ui.set_machine_on(p.machine_on.get());
                ui.set_estop(p.estop.get());
                ui.set_output_on(p.output.get());
                ui.set_heartbeat(beat);
            }
        });
    }

    ui.run()
}
