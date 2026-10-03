//! 嵌入的图片资源：由 `build.rs` 从仓库根目录的 `assets/images/*.ppm` 生成

include!(concat!(env!("OUT_DIR"), "/images.rs"));
