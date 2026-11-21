/*
 * Streamer: the slate of a source — what its lives send while it is lost. Its own image, chosen
 * among those dropped here (drag and drop, or a click to pick files), else its name on a dark
 * picture. An image is deleted through a confirmation window.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Check, ImagePlus, Trash2, X } from "lucide-react";
import { t, tn } from "../i18n";
import { api, can, useApp, useChanged } from "../store";
import { confirmDialog } from "../ui/dialogs";
import type { Recorder } from "../types";

interface SlateImage {
  id: string;
  name: string;
  ext: string;
  added: number;
  by: string;
}
interface Slates {
  images: SlateImage[];
  sources: Record<string, string>;
}

const src = (id: string) => `/api/slates/${id}/image`;

function useSlates(): [Slates | null, () => Promise<void>] {
  const [s, setS] = useState<Slates | null>(null);
  const load = useCallback(async () => {
    try {
      setS(await api<Slates>("GET", "/api/slates"));
    } catch {
      /* the next change */
    }
  }, []);
  useEffect(() => void load(), [load]);
  useChanged("slates", () => void load());
  return [s, load];
}

/** The default slate, drawn small: the source's name on a dark picture. */
function DefaultSlate({ name }: { name: string }) {
  return (
    <span className="slate-default">
      <b>{name}</b>
      <span>{t("The live comes back in a moment")}</span>
    </span>
  );
}

/** The panel's line: the slate of this source, and the way to change it. */
export function SlateRow({ rec }: { rec: Recorder }) {
  const [slates] = useSlates();
  const allowed = useApp((s) => can.has(s.auth?.user, "stream.run"));
  const [open, setOpen] = useState(false);
  const id = slates?.sources[rec.id];
  const img = slates?.images.find((x) => x.id === id);
  return (
    <div className="slate-row">
      <div className="slate-thumb">
        {img ? (
          <img src={src(img.id)} alt="" />
        ) : (
          <DefaultSlate name={rec.name} />
        )}
      </div>
      <div className="slate-row-text">
        <span>{t("Slate")}</span>
        <b title={img?.name}>{img ? img.name : t("Default")}</b>
      </div>
      {allowed && (
        <button
          type="button"
          className="btn small"
          onClick={() => setOpen(true)}
        >
          {t("Change")}
        </button>
      )}
      {/* Over the whole page, not inside the panel (its styles stay its own). */}
      {open &&
        createPortal(
          <SlateModal rec={rec} onClose={() => setOpen(false)} />,
          document.body,
        )}
    </div>
  );
}

function SlateModal({ rec, onClose }: { rec: Recorder; onClose: () => void }) {
  const [slates, reload] = useSlates();
  const toast = useApp((s) => s.toast);
  const [over, setOver] = useState(false);
  const [sending, setSending] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  const chosen = slates?.sources[rec.id] ?? null;

  const choose = async (image: string | null) => {
    try {
      await api("PUT", `/api/slates/source/${rec.id}`, { image });
      await reload();
    } catch (e) {
      toast(t((e as Error).message), "error");
    }
  };

  const send = async (files: FileList | File[]) => {
    const list = [...files].filter((f) =>
      /^image\/(png|jpeg|webp)$/.test(f.type),
    );
    if (list.length < [...files].length)
      toast(t("the image is a PNG, a JPEG or a WebP"), "error");
    let last: string | null = null;
    for (const f of list) {
      setSending((n) => n + 1);
      try {
        const name = f.name.replace(/\.[^.]+$/, "");
        const img = await api<SlateImage>(
          "POST",
          `/api/slates?name=${encodeURIComponent(name)}`,
          f,
        );
        last = img.id;
      } catch (e) {
        toast(`${f.name} : ${t((e as Error).message)}`, "error");
      } finally {
        setSending((n) => n - 1);
      }
    }
    // One image sent: it becomes this source's slate.
    if (list.length === 1 && last) await choose(last);
    else await reload();
  };

  const remove = async (img: SlateImage) => {
    const users = Object.values(slates?.sources ?? {}).filter(
      (x) => x === img.id,
    ).length;
    const ok = await confirmDialog({
      title: t("Delete the image “{name}”?", { name: img.name }),
      message: users
        ? tn(
            users,
            "A source uses it: it goes back to the default slate.",
            "{n} sources use it: they go back to the default slate.",
          )
        : t("No source uses it."),
      confirm: t("Delete"),
      danger: true,
    });
    if (!ok) return;
    try {
      await api("DELETE", `/api/slates/${img.id}`);
      await reload();
    } catch (e) {
      toast(t((e as Error).message), "error");
    }
  };

  return (
    <div className="modal-back" onPointerDown={onClose}>
      <div
        className="modal wide slate-modal"
        onPointerDown={(e) => e.stopPropagation()}
      >
        <header>
          <h2>{t("Slate of “{name}”", { name: rec.name })}</h2>
          <button
            type="button"
            className="icon-btn ghost"
            title={t("Close")}
            onClick={onClose}
          >
            <X size={16} />
          </button>
        </header>
        <div
          className={`slate-drop ${over ? "over" : ""}`}
          role="button"
          tabIndex={0}
          onClick={() => input.current?.click()}
          onKeyDown={(e) =>
            (e.key === "Enter" || e.key === " ") && input.current?.click()
          }
          onDragOver={(e) => {
            e.preventDefault();
            setOver(true);
          }}
          onDragLeave={() => setOver(false)}
          onDrop={(e) => {
            e.preventDefault();
            setOver(false);
            void send(e.dataTransfer.files);
          }}
        >
          <ImagePlus size={22} />
          <b>
            {sending > 0
              ? t("Sending…")
              : t("Drop images here, or click to choose")}
          </b>
          <span>{t("PNG, JPEG or WebP, 10 MB at most · 16:9 fits best")}</span>
          <input
            ref={input}
            type="file"
            accept="image/png,image/jpeg,image/webp"
            multiple
            hidden
            onChange={(e) => {
              if (e.target.files) void send(e.target.files);
              e.target.value = "";
            }}
          />
        </div>
        <div className="slate-grid">
          <button
            type="button"
            className={`slate-tile ${chosen === null ? "on" : ""}`}
            onClick={() => void choose(null)}
          >
            <span className="slate-pic">
              <DefaultSlate name={rec.name} />
            </span>
            <span className="slate-name">{t("Default")}</span>
            {chosen === null && (
              <span className="slate-check">
                <Check size={12} />
              </span>
            )}
          </button>
          {(slates?.images ?? []).map((img) => (
            <div
              key={img.id}
              className={`slate-tile ${chosen === img.id ? "on" : ""}`}
              role="button"
              tabIndex={0}
              onClick={() => void choose(img.id)}
              onKeyDown={(e) => e.key === "Enter" && void choose(img.id)}
            >
              <span className="slate-pic">
                <img src={src(img.id)} alt="" loading="lazy" />
              </span>
              <span className="slate-name" title={img.name}>
                {img.name}
              </span>
              {chosen === img.id && (
                <span className="slate-check">
                  <Check size={12} />
                </span>
              )}
              <button
                type="button"
                className="slate-del icon-btn ghost danger"
                data-tip={t("Delete")}
                aria-label={t("Delete")}
                onClick={(e) => {
                  e.stopPropagation();
                  void remove(img);
                }}
              >
                <Trash2 size={13} />
              </button>
            </div>
          ))}
        </div>
        <footer>
          <button type="button" className="btn" onClick={onClose}>
            {t("Close")}
          </button>
        </footer>
      </div>
    </div>
  );
}
