import type { ReactNode } from "react";
import {
  AlertCircle,
  ArrowRight,
  Check,
  ChevronRight,
  Terminal,
  Unplug,
} from "lucide-react";
import { Button } from "./components/arc/button/button";
import { CopyButton } from "./components/arc/copy-button/copy-button";
export { Button };
export { Input } from "./components/arc/input/input";
export { Switch } from "./components/arc/switch/switch";
export { Dialog, DialogContent } from "./components/arc/dialog/dialog";
export { default as SegmentedControl } from "./components/arc/segmented-control/segmented-control";
export function Logo({ compact = false }: { compact?: boolean }) {
  return (
    <span className="brand">
      <img src="/favicon.svg" alt="" />
      <span>MCPort{!compact && <small>by Silicon</small>}</span>
    </span>
  );
}
export function Status({ status = "unknown" }: { status?: string }) {
  const positive = [
    "online",
    "connected",
    "success",
    "succeeded",
    "completed",
    "ready",
  ].includes(status);
  const negative = ["offline", "failed", "error", "denied"].includes(status);
  return (
    <span
      className={`status ${positive ? "positive" : negative ? "negative" : "neutral"}`}
    >
      <i />
      {status.replaceAll("_", " ")}
    </span>
  );
}
export function ErrorBox({
  error,
  onRetry,
}: {
  error: string;
  onRetry?: () => void;
}) {
  return (
    <div role="alert" className="error-box">
      <AlertCircle size={18} />
      <div>
        {error}
        {onRetry && (
          <button className="text-button" onClick={onRetry}>
            Try again <ArrowRight size={14} />
          </button>
        )}
      </div>
    </div>
  );
}
export function Empty({
  title,
  description,
  action,
  icon,
}: {
  title: string;
  description: string;
  action?: ReactNode;
  icon?: ReactNode;
}) {
  return (
    <div className="empty">
      <span className="empty-icon">{icon || <Unplug size={24} />}</span>
      <h3>{title}</h3>
      <p>{description}</p>
      {action}
    </div>
  );
}
export function Loading() {
  return (
    <div role="status" aria-label="Loading" className="loading">
      <span />
      <span />
      <span />
    </div>
  );
}
export function Code({
  children,
  label = "Terminal",
}: {
  children: string;
  label?: string;
}) {
  return (
    <div className="code-block">
      <div className="code-top">
        <span>
          <Terminal size={13} />
          {label}
        </span>
        <CopyButton value={children} iconOnly variant="plain" />
      </div>
      <pre>{children}</pre>
    </div>
  );
}
export function PageHead({
  eyebrow,
  title,
  description,
  action,
}: {
  eyebrow?: string;
  title: string;
  description: string;
  action?: ReactNode;
}) {
  return (
    <div className="page-head">
      <div>
        {eyebrow && <div className="eyebrow">{eyebrow}</div>}
        <h1>{title}</h1>
        <p>{description}</p>
      </div>
      {action}
    </div>
  );
}
export function Select({
  label,
  value,
  onChange,
  children,
  description,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  children: ReactNode;
  description?: string;
}) {
  return (
    <label className="select-field">
      <span>{label}</span>
      <select value={value} onChange={(e) => onChange(e.target.value)}>
        {children}
      </select>
      {description && <small>{description}</small>}
    </label>
  );
}
export function Textarea({
  label,
  value,
  onChange,
  placeholder,
  code = false,
  rows = 5,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  code?: boolean;
  rows?: number;
}) {
  return (
    <label className="textarea-field">
      <span>{label}</span>
      <textarea
        spellCheck={!code}
        className={code ? "code-input" : ""}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        rows={rows}
        placeholder={placeholder}
      />
    </label>
  );
}
export function Notice({ children }: { children: ReactNode }) {
  return (
    <div className="notice">
      <Check size={16} />
      <span>{children}</span>
    </div>
  );
}
export function Breadcrumb({
  name,
  onBack,
}: {
  name: string;
  onBack: () => void;
}) {
  return (
    <div className="breadcrumb">
      <button onClick={onBack}>Connections</button>
      <ChevronRight size={14} />
      <span>{name}</span>
    </div>
  );
}
