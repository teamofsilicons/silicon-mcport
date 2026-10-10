"use client";
import { useEffect, useMemo, useState } from "react";
import {
  Download,
  ExternalLink,
  FileText,
  Link as LinkIcon,
} from "lucide-react";

import { api, message } from "./lib/api";
import type { ResultAsset } from "./lib/api";
import { Button, ErrorBox, Loading } from "./ui";
type Content = Record<string, unknown>;
export function safeResourceLink(value: unknown): string | null {
  if (typeof value !== "string") return null;
  try {
    const url = new URL(value);
    if (
      !["https:", "http:"].includes(url.protocol) ||
      url.username ||
      url.password
    )
      return null;
    const h = url.hostname.toLowerCase();
    if (
      h === "localhost" ||
      h.endsWith(".localhost") ||
      h.startsWith("127.") ||
      h === "[::1]" ||
      h === "[::]" ||
      /^\[(fc|fd|fe[89ab])/.test(h) ||
      /^0\.|^10\.|^169\.254\.|^192\.168\.|^172\.(1[6-9]|2\d|3[01])\./.test(h)
    )
      return null;
    return url.href;
  } catch {
    return null;
  }
}
const imageMimes = new Set([
  "image/png",
  "image/jpeg",
  "image/webp",
  "image/gif",
]);
const audioMimes = new Set([
  "audio/mpeg",
  "audio/mp3",
  "audio/wav",
  "audio/ogg",
  "audio/flac",
  "audio/mp4",
]);
const suffix: Record<string, string> = {
  "image/png": "png",
  "image/jpeg": "jpg",
  "image/webp": "webp",
  "image/gif": "gif",
  "audio/mpeg": "mp3",
  "audio/mp3": "mp3",
  "audio/wav": "wav",
  "audio/ogg": "ogg",
  "audio/flac": "flac",
  "audio/mp4": "m4a",
  "application/pdf": "pdf",
  "text/plain": "txt",
};
function useFile(data: unknown, mime: unknown) {
  const url = useMemo(() => {
    if (typeof data !== "string" || data.length > 24 * 1024 * 1024) return null;
    try {
      const bytes = Uint8Array.from(atob(data), (char) => char.charCodeAt(0));
      return URL.createObjectURL(
        new Blob([bytes], {
          type: typeof mime === "string" ? mime : "application/octet-stream",
        }),
      );
    } catch {
      return null;
    }
  }, [data, mime]);
  useEffect(
    () => () => {
      if (url) URL.revokeObjectURL(url);
    },
    [url],
  );
  return url;
}
function Media({
  content,
  index,
  download = true,
}: {
  content: Content;
  index: number;
  download?: boolean;
}) {
  const mime =
    typeof content.mimeType === "string"
      ? content.mimeType
      : "application/octet-stream";
  const url = useFile(content.data ?? content.blob, mime);
  if (!url)
    return (
      <p className="field-help">
        This content cannot be previewed. Its original data is preserved in the
        full JSON result.
      </p>
    );
  const isImage = imageMimes.has(mime);
  const isAudio = audioMimes.has(mime);
  return (
    <div className="media-result">
      {isImage ? (
        // MCP images are validated blob URLs, not remote Next Image sources.
        // eslint-disable-next-line @next/next/no-img-element
        <img
          src={url}
          alt={
            typeof content.name === "string"
              ? content.name
              : `MCP result image ${index + 1}`
          }
        />
      ) : isAudio ? (
        <audio controls src={url} preload="none" />
      ) : (
        <div className="file-result">
          <FileText size={22} />
          <span>{mime}</span>
        </div>
      )}
      {download && (
        <a
          className="text-button"
          href={url}
          download={`mcport-result-${index + 1}.${suffix[mime] || "bin"}`}
        >
          <Download size={13} />
          Download {isImage ? "image" : isAudio ? "audio" : "file"}
        </a>
      )}
    </div>
  );
}
function Block({
  content,
  index,
  copiedUris = new Set<string>(),
}: {
  content: Content;
  index: number;
  copiedUris?: Set<string>;
}) {
  if (content.type === "text" && typeof content.text === "string")
    return <pre className="text-result">{content.text}</pre>;
  if (content.type === "image" || content.type === "audio")
    return <Media content={content} index={index} />;
  if (
    content.type === "resource" &&
    content.resource &&
    typeof content.resource === "object"
  ) {
    const resource = content.resource as Content;
    return (
      <section className="resource-result">
        <header>
          <FileText size={14} />
          <code>{String(resource.uri || "Embedded resource")}</code>
        </header>
        {typeof resource.text === "string" && (
          <pre className="text-result">{resource.text}</pre>
        )}
        {typeof resource.blob === "string" && (
          <Media content={resource} index={index} />
        )}
      </section>
    );
  }
  if (content.type === "resource_link") {
    const href = safeResourceLink(content.uri);
    return (
      <section className="resource-result">
        <header>
          <LinkIcon size={14} />
          <strong>
            {String(content.title || content.name || "Resource link")}
          </strong>
        </header>
        {typeof content.description === "string" && (
          <p>{content.description}</p>
        )}
        {copiedUris.has(String(content.uri)) ? (
          <p className="field-help">
            Copied from its execution host. Download it from Result files below.
          </p>
        ) : href ? (
          <a
            href={href}
            className="text-button"
            target="_blank"
            rel="noopener noreferrer"
          >
            Open resource <ExternalLink size={13} />
          </a>
        ) : (
          <p className="field-help">
            This resource needs its provider’s authorized retrieval path. A
            local file or private endpoint cannot be opened from another
            machine.
          </p>
        )}
        <code className="resource-uri">{String(content.uri || "")}</code>
      </section>
    );
  }
  return <pre className="json-result">{JSON.stringify(content, null, 2)}</pre>;
}
export function McpResult({
  result,
  callId,
}: {
  result: unknown;
  callId?: string;
}) {
  const data = (
    result && typeof result === "object" ? result : { value: result }
  ) as Content;
  const content = Array.isArray(data.content)
    ? (data.content.filter((c) => c && typeof c === "object") as Content[])
    : [];
  const contents = Array.isArray(data.contents)
    ? (data.contents.filter((c) => c && typeof c === "object") as Content[])
    : [];
  const meta =
    data._meta && typeof data._meta === "object" ? (data._meta as Content) : {};
  const mcport =
    meta.mcport && typeof meta.mcport === "object"
      ? (meta.mcport as Content)
      : {};
  const copied = Array.isArray(mcport.assets)
    ? mcport.assets.filter((a): a is Content => !!a && typeof a === "object")
    : [];
  const copiedUris = new Set(copied.map((a) => String(a.uri)));
  const hasPreview =
    content.length > 0 ||
    contents.length > 0 ||
    data.structuredContent !== undefined;
  return (
    <div className="mcp-result">
      {content.map((c, index) => (
        <Block key={index} content={c} index={index} copiedUris={copiedUris} />
      ))}
      {contents.map((c, index) => (
        <Block
          key={`resource-${index}`}
          content={{ type: "resource", resource: c }}
          index={index}
        />
      ))}
      {data.structuredContent !== undefined && (
        <section>
          <h4>Structured output</h4>
          <pre className="json-result">
            {JSON.stringify(data.structuredContent, null, 2)}
          </pre>
        </section>
      )}
      {copied.length > 0 && (
        <section className="copied-assets">
          <h4>Files from the execution host</h4>
          <p className="field-help">
            These files were copied with this call, so they’re available from
            your machine.
          </p>
          {copied.map((asset, index) => (
            <Media key={index} content={asset} index={index} download={false} />
          ))}
        </section>
      )}
      {typeof mcport.unavailable_asset_count === "number" &&
        mcport.unavailable_asset_count > 0 && (
          <p className="field-help">
            Some returned links could not be copied. The original result is
            preserved; ask the provider for an MCP resource when a link requires
            additional access.
          </p>
        )}
      {callId && <ResultFiles callId={callId} />}
      <details open={!hasPreview}>
        <summary>Full JSON result</summary>
        <pre className="json-result">{JSON.stringify(result, null, 2)}</pre>
      </details>
    </div>
  );
}

function ResultFiles({ callId }: { callId: string }) {
  const [files, setFiles] = useState<ResultAsset[] | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState<number | null>(null);
  useEffect(() => {
    let active = true;
    setFiles(null);
    setError("");
    api
      .assets(callId)
      .then((items) => {
        if (active) setFiles(items);
      })
      .catch((e) => {
        if (active) setError(message(e));
      });
    return () => {
      active = false;
    };
  }, [callId]);
  const download = async (file: ResultAsset) => {
    setBusy(file.index);
    setError("");
    try {
      const url = await api.downloadAsset(callId, file.index);
      const link = document.createElement("a");
      link.href = url;
      link.download = file.name;
      link.style.display = "none";
      document.body.append(link);
      link.click();
      link.remove();

    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(null);
    }
  };
  if (files?.length === 0) return null;
  return (
    <section className="result-files">
      <h4>Result files</h4>
      <p className="field-help">
        Downloads check your current access to this connection and action.
      </p>
      {error && <ErrorBox error={error} />}
      {!files && !error && <Loading />}
      {files?.map((file) => (
        <div className="result-file-row" key={file.index}>
          <FileText size={18} />
          <div>
            <strong>{file.name}</strong>
            <small>
              {file.mime_type} ·{" "}
              {file.size < 1024
                ? `${file.size} B`
                : file.size < 1048576
                  ? `${(file.size / 1024).toFixed(1)} KB`
                  : `${(file.size / 1048576).toFixed(1)} MB`}
            </small>
          </div>
          <Button
            variant="secondary"
            size="sm"
            loading={busy === file.index}
            disabled={busy !== null}
            onClick={() => void download(file)}
            aria-label={`Download ${file.name}`}
          >
            <Download size={14} />
            Download
          </Button>
        </div>
      ))}
    </section>
  );
}
