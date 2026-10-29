// replay_retry <file.ll> [vertex|fragment|kernel] [retry.ll]: translate a module the way the plugin's nvmtl_translate does
// (as written, then the pointer retry with helpers inlined and object fields forwarded), printing each attempt's result.
// The input is a module kept by the plugin's NVMTL_XLATE_FAIL_DUMP / NVMTL_FC_FAIL_DUMP (<dir>/fc-<pid>-<n>.ll).
// With a third argument the retry's input text is written there, with a fourth the SPIR-V produced.
use metal2vulkan::passes::{Stage, TransformOptions};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let ll = std::fs::read_to_string(a.get(1).expect("file.ll")).expect("read");
    let st = match a.get(2).map(String::as_str).unwrap_or("fragment") {
        "vertex" => Stage::Vertex, "kernel" => Stage::Kernel, _ => Stage::Fragment };
    let first = metal2vulkan::translate_native_no_retry_constructed_with_options(&ll, st, TransformOptions::default());
    match &first {
        Ok(v) => { println!("as written: OK ({} bytes)", v.len()); if let Some(p) = a.get(4) { std::fs::write(p, v).expect("write spv"); } return; }
        Err(e) => println!("as written: {e}"),
    }
    let inlined = metal2vulkan::inline_and_forward_object_fields(&ll);
    println!("inlined: {}", if inlined.is_some() { "yes" } else { "no (retry on the text as written)" });
    let retry = inlined.as_deref().unwrap_or(&ll);
    if let Some(p) = a.get(3) { std::fs::write(p, retry).expect("write retry"); }
    match metal2vulkan::with_argument_buffer_samplers(|| {
        metal2vulkan::translate_native_no_retry_constructed_with_options(retry, st, TransformOptions::default())
    }) {
        Ok(v) => { println!("retry: OK ({} bytes)", v.len()); if let Some(p) = a.get(4) { std::fs::write(p, &v).expect("write spv"); } }
        Err(e) => { println!("retry: {e}"); std::process::exit(1) }
    }
}
