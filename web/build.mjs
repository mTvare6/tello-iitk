import { execFileSync } from "node:child_process";
import { readFile, mkdir, cp, rm } from "node:fs/promises";
import { resolve } from "node:path";
import { build } from "esbuild";

const run = (command, args) => execFileSync(command, args, { stdio: "inherit" });
run("rustup", ["target", "add", "--toolchain", "stable", "wasm32-unknown-unknown"]);
run("cargo", ["+stable", "build", "--locked", "--release", "--lib", "--target", "wasm32-unknown-unknown"]);
const lock = await readFile("Cargo.lock", "utf8");
const version = lock.match(/name = "wasm-bindgen"\nversion = "([^"]+)"/)[1];
let installed;
try { installed = execFileSync("wasm-bindgen", ["--version"], { encoding: "utf8" }).trim(); } catch (_) {}
if (installed !== `wasm-bindgen ${version}`) run("cargo", ["install", "wasm-bindgen-cli", "--version", version, "--locked"]);
const target = process.env.CARGO_TARGET_DIR || "target";
const bindings = resolve(target, "browser-bindgen");
run("wasm-bindgen", [resolve(target, "wasm32-unknown-unknown/release/hello_iitk.wasm"), "--target", "web", "--out-dir", bindings]);
await rm("dist", { recursive: true, force: true });
await mkdir("dist/ort", { recursive: true });
await cp("web/index.html", "dist/index.html");
await cp("web/style.css", "dist/style.css");
await cp("web/app.js", "dist/app.js");
await mkdir("dist/assets", { recursive: true });
for (const name of ["captcha-glyph.onnx", "captcha-cnn.onnx"]) await cp(`assets/${name}`, `dist/assets/${name}`);
await cp(resolve(bindings, "hello_iitk_bg.wasm"), "dist/hello_iitk_bg.wasm");
for (const name of ["ort-wasm-simd-threaded.mjs", "ort-wasm-simd-threaded.wasm"]) await cp(`node_modules/onnxruntime-web/dist/${name}`, `dist/ort/${name}`);
await build({ entryPoints: ["web/client.js"], bundle: true, format: "esm", target: "es2022", outfile: "dist/client.js", alias: { __hello_wasm: resolve(bindings, "hello_iitk.js") }, conditions: ["onnxruntime-web-use-extern-wasm"], minify: true });
console.log("Built web assets in dist/");
