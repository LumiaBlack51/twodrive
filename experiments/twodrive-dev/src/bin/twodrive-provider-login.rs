use clap::Parser;
use std::{io::IsTerminal, path::PathBuf};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    state: PathBuf,
    #[arg(long, hide = true)]
    interactive_console: bool,
}

#[cfg(windows)]
fn interactive_console() -> anyhow::Result<Vec<std::fs::File>> {
    use std::{fs::OpenOptions, os::windows::io::AsRawHandle};
    use windows_sys::Win32::System::Console::{
        STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
    };
    let mut handles = Vec::new();
    for (name, kind) in [
        ("CONIN$", STD_INPUT_HANDLE),
        ("CONOUT$", STD_OUTPUT_HANDLE),
        ("CONOUT$", STD_ERROR_HANDLE),
    ] {
        let file = OpenOptions::new().read(true).write(true).open(name)?;
        anyhow::ensure!(
            unsafe { SetStdHandle(kind, file.as_raw_handle()) } != 0,
            "无法连接交互控制台"
        );
        handles.push(file);
    }
    Ok(handles)
}

fn main() {
    let args = Args::parse();
    #[cfg(windows)]
    let _console = if args.interactive_console {
        match interactive_console() {
            Ok(handles) => Some(handles),
            Err(_) => std::process::exit(1),
        }
    } else {
        None
    };
    let result =
        twodrive_dev::state::State::new(args.state).and_then(twodrive_dev::rclone_drive::configure);
    let failed = result.is_err();
    if let Err(error) = result {
        eprintln!("登录未完成：{error}");
    } else {
        println!("iCloud 登录已保存。请回到 TwoDrive，选择该连接并点击“登录并使用”。");
    }
    if std::io::stdin().is_terminal() {
        println!("按回车关闭此登录窗口。");
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
    }
    if failed {
        std::process::exit(1);
    }
}
