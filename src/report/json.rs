//! JSON 输出。
//!
//! **这是一等公民，不是附属品。** agent 本身就是这个工具的目标用户之一，
//! 它们该读结构化结果，而不是去正则匹配人类可读的表格。

use crate::model::ScanReport;

pub fn render(report: &ScanReport) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(report)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use crate::model::*;

    fn empty() -> ScanReport {
        ScanReport { repos: Vec::new(), available_bytes: 1234, tools: Vec::new() }
    }

    #[test]
    fn empty_report_round_trips() {
        let s = render(&empty()).expect("应能序列化");
        let v: serde_json::Value = serde_json::from_str(&s).expect("应能解析回来");
        assert_eq!(v["available_bytes"], 1234);
    }

    #[test]
    fn verdict_and_cause_survive_serialization() {
        // 消费者要能区分「拦下」与「判不准」，以及判不准的具体成因。
        // 这两个信息一旦在序列化里塌掉，JSON 输出就失去意义。
        let v = serde_json::to_value(Verdict::NeedsAttention { unknown: vec![GateId::Busy] })
            .expect("序列化 Verdict");
        assert!(v.get("NeedsAttention").is_some(), "判定的种类必须可辨识: {v}");

        let c = serde_json::to_value(Cause::ToolMissing { tool: "gh" }).expect("序列化 Cause");
        assert_eq!(c["ToolMissing"]["tool"], "gh", "成因的细节必须保留: {c}");
    }
}
