use std::path::Path;

/// 前端未构建时生成占位 index.html，保证 rust-embed 的目录存在、server 可编译。
fn main() {
    let dist = Path::new("../../web/dist");
    let index = dist.join("index.html");
    if !index.exists() {
        let _ = std::fs::create_dir_all(dist);
        let _ = std::fs::write(
            &index,
            "<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\"><title>minimon</title></head>\
             <body><p>前端尚未构建，请先执行：cd web &amp;&amp; npm ci &amp;&amp; npm run build，然后重新编译 server。</p></body></html>\n",
        );
    }
    println!("cargo:rerun-if-changed=../../web/dist");
}
