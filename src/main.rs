use std::process::ExitCode;

use crwm::run;

fn main() -> ExitCode {
    // run() 把回调里收集到的错误一路带到这里统一处理,
    // 而不是在 event 回调里 panic 或 process::exit。
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("crwm: {err}");
            ExitCode::FAILURE
        }
    }
}
