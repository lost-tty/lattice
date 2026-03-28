//! JavaScript unit tests for the web UI.
//!
//! Uses boa_engine to run `.test.js` files from `static/tests/`.
//! Test logic lives in JS — Rust just loads each module, checks results.
//! No browser, no Node.js, no npm needed.

use boa_engine::module::SimpleModuleLoader;
use boa_engine::{js_string, Context, Module, Source};
use std::path::Path;
use std::rc::Rc;

fn static_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("static")
}

/// Run a JS test file from `static/tests/`, return (passed, failed, error messages).
fn run_js_test(filename: &str) -> (i32, i32, Vec<String>) {
    let loader = Rc::new(SimpleModuleLoader::new(static_dir()).unwrap());
    let mut ctx = Context::builder()
        .module_loader(loader.clone())
        .build()
        .unwrap();

    // Shim browser globals
    ctx.eval(Source::from_bytes(
        r#"
        var location = { pathname: '/', hostname: 'localhost', port: '8080', protocol: 'http:' };
        var history = { pushState: function(){}, replaceState: function(){} };
        var window = { addEventListener: function(){} };
        var TextEncoder = function() {};
        TextEncoder.prototype.encode = function(s) {
            var arr = [];
            for (var i = 0; i < s.length; i++) arr.push(s.charCodeAt(i) & 0xff);
            return new Uint8Array(arr);
        };
        var TextDecoder = function() {};
        TextDecoder.prototype.decode = function(u8) {
            var s = '';
            for (var i = 0; i < u8.length; i++) s += String.fromCharCode(u8[i]);
            return s;
        };
        "#,
    ))
    .unwrap();

    // Load and evaluate the test module
    let path = static_dir().join("tests").join(filename);
    let module = Module::parse(Source::from_filepath(&path).unwrap(), None, &mut ctx).unwrap();

    // Register with the loader so relative imports resolve from the test file's directory
    loader.insert(path.canonicalize().unwrap(), module.clone());

    let promise = module.load_link_evaluate(&mut ctx);
    ctx.run_jobs().unwrap();

    match promise.state() {
        boa_engine::builtins::promise::PromiseState::Fulfilled(_) => {}
        state => panic!("{filename} failed to load: {state:?}"),
    }

    // Read the default export (results object)
    let results = module.get_value(js_string!("default"), &mut ctx).unwrap();
    let results_obj = results
        .as_object()
        .expect("default export should be an object");

    let passed = results_obj
        .get(js_string!("passed"), &mut ctx)
        .unwrap()
        .to_i32(&mut ctx)
        .unwrap();
    let failed = results_obj
        .get(js_string!("failed"), &mut ctx)
        .unwrap()
        .to_i32(&mut ctx)
        .unwrap();

    let errors_val = results_obj.get(js_string!("errors"), &mut ctx).unwrap();
    let errors_arr = errors_val.as_object().unwrap();
    let len = errors_arr
        .get(js_string!("length"), &mut ctx)
        .unwrap()
        .to_i32(&mut ctx)
        .unwrap();

    let mut errors = Vec::new();
    for i in 0..len {
        let msg = errors_arr
            .get(i as u32, &mut ctx)
            .unwrap()
            .to_string(&mut ctx)
            .unwrap()
            .to_std_string_escaped();
        errors.push(msg);
    }

    (passed, failed, errors)
}

#[test]
fn js_router_tests() {
    let (passed, failed, errors) = run_js_test("router.test.js");
    if failed > 0 {
        panic!(
            "router.test.js: {passed} passed, {failed} failed\n{}",
            errors.join("\n")
        );
    }
    assert!(passed > 0, "no tests ran");
    eprintln!("router.test.js: {passed} passed");
}

#[test]
fn js_helpers_tests() {
    let (passed, failed, errors) = run_js_test("helpers.test.js");
    if failed > 0 {
        panic!(
            "helpers.test.js: {passed} passed, {failed} failed\n{}",
            errors.join("\n")
        );
    }
    assert!(passed > 0, "no tests ran");
    eprintln!("helpers.test.js: {passed} passed");
}
