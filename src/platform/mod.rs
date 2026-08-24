//! 平台相关能力。拿不到的能力一律向上报 `Cause::Unsupported`，
//! 由门禁落成 `Unknown` —— **绝不返回一个「看起来正常」的空结果**。

pub mod disk;
pub mod procs;
