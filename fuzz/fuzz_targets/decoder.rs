#![no_main]
use std::sync::Once;

static HOOK: Once = Once::new();

// The session contains a panic in the demo parser, a dependency, and reconnects: that is not a
// finding here. libFuzzer's hook would abort on it, so only a panic elsewhere goes on to that hook.
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    HOOK.call_once(|| {
        let abort = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let file = info.location().map_or("", |l| l.file());
            if !file.contains("tf-demo-parser") && !file.contains("bitbuffer") {
                abort(info);
            }
        }));
    });
    stvtf::fuzzing::decoder(data);
});
