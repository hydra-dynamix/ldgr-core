use std::any::Any;
use std::io::ErrorKind;
use std::panic;

fn main() {
    let default_panic_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        if !is_broken_pipe_panic(info.payload()) {
            default_panic_hook(info);
        }
    }));

    let result = panic::catch_unwind(ldgr_core::cli::run);
    let result = match result {
        Ok(result) => result,
        Err(payload) if is_broken_pipe_panic(payload.as_ref()) => return,
        Err(payload) => panic::resume_unwind(payload),
    };

    if let Err(error) = result {
        if error.chain().any(|cause| {
            cause
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == ErrorKind::BrokenPipe)
        }) {
            return;
        }
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn is_broken_pipe_panic(payload: &(dyn Any + Send)) -> bool {
    let Some(message) = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
    else {
        return false;
    };

    message.contains("failed printing to stdout") && message.contains("Broken pipe")
}
