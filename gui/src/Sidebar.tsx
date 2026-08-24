import type { ReactNode } from "react";

/**
 * 右侧可收起的工程栏。
 *
 * 仓库管理原本排在主内容下方，等于把「配置」和「今天要处理什么」混在一条时间线上，
 * 而这两件事的节奏完全不同：配置几周动一次，处置每天看一次。
 * 拆到侧栏后，主区专心回答「现在该做什么」，配置随手可开、平时不占版面。
 */
export function Sidebar({
  open,
  onToggle,
  title,
  action,
  children,
}: {
  open: boolean;
  onToggle: () => void;
  title: string;
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <aside className={`sidebar ${open ? "open" : ""}`}>
      {/* 收起后仍要有一个够大的把手，否则用户找不到怎么把它叫回来 */}
      <button
        className="sidebar-handle"
        onClick={onToggle}
        aria-expanded={open}
        title={open ? "收起" : title}
      >
        <span className={`chev ${open ? "open" : ""}`}>‹</span>
      </button>

      <div className="sidebar-inner">
        <div className="sidebar-head">
          <h2>{title}</h2>
          {action}
        </div>
        <div className="sidebar-body">{children}</div>
      </div>
    </aside>
  );
}
