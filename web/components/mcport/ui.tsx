"use client";
import { Children, isValidElement, type ReactNode, type ComponentProps } from "react";
import { PageHeader } from "@/components/foundation/layout/layout";
import { Select as ArcSelect } from "@/components/silicon-ui/select/select";
import { Textarea as ArcTextarea } from "@/components/silicon-ui/textarea/textarea";
import { DialogContent as ArcDialogContent } from "@/components/silicon-ui/dialog/dialog";
import {
  AlertCircle,
  ArrowRight,
  Check,
  ChevronRight,
  Terminal,
  Unplug,
} from "lucide-react";
import { Button } from "@/components/silicon-ui/button/button";
import { CopyButton } from "@/components/silicon-ui/copy-button/copy-button";
export { Button };
export { Input } from "@/components/silicon-ui/input/input";
export { Switch } from "@/components/silicon-ui/switch/switch";
export { Dialog } from "@/components/silicon-ui/dialog/dialog";
export function DialogContent(props:ComponentProps<typeof ArcDialogContent>){return <ArcDialogContent {...props} className={"mcport-product "+(props.className??"")}/>;}
export { default as SegmentedControl } from "@/components/silicon-ui/segmented-control/segmented-control";
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
  return <PageHeader title={title} description={description} actions={action}>{eyebrow?<span className="eyebrow">{eyebrow}</span>:null}</PageHeader>;
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
  const options=Children.toArray(children).flatMap(child=>{
    if(!isValidElement<{value:string;children:ReactNode;disabled?:boolean}>(child))return [];
    return [{value:String(child.props.value)||"__empty",label:String(child.props.children),disabled:child.props.disabled}];
  });
  return <ArcSelect label={label} value={value||"__empty"} options={options} onValueChange={v=>onChange(v==="__empty"?"":v)} description={description}/>;
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
  return <ArcTextarea label={label} value={value} onChange={e=>onChange(e.target.value)} placeholder={placeholder} rows={rows} spellCheck={!code}/>;
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
