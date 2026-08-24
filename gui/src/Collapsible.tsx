import type { ReactNode } from "react";

/**
 * 可平滑展开/收起的区块。
 *
 * 用 grid-template-rows 从 0fr 过渡到 1fr，而不是 max-height 猜一个够大的值——
 * 后者要么内容超出被截断，要么值给太大导致收起时先"空转"一段才动起来，
 * 而这里的内容长度完全取决于用户有多少个仓库，猜不出来。
 */
export function Collapsible({
  open,
  title,
  count,
  onToggle,
  action,
  children,
}: {
  open: boolean;
  title: string;
  count?: number;
  onToggle: () => void;
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="collapsible">
      <div className="collapsible-head">
        <button
          className="collapsible-toggle"
          onClick={onToggle}
          aria-expanded={open}
        >
          <span className={`chev ${open ? "open" : ""}`}>›</span>
          {title}
          {count !== undefined && <em>{count}</em>}
        </button>
        {action}
      </div>
      <div className={`collapsible-body ${open ? "open" : ""}`}>
        <div>{children}</div>
      </div>
    </section>
  );
}
