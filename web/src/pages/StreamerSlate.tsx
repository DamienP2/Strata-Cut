/*
 * Streamer: the pictures of a source's lives — the slate (sent while the source is lost), the
 * programme's image (CUT, AUTO) and its overlays (transparent PNG). Chosen in a window among
 * those dropped there (drag and drop, or a click to pick files); one is deleted through a
 * confirmation window. Without a slate or an image of its own, a source has its name on a dark
 * picture.
 */
import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { Check, ImagePlus, Trash2, X } from "lucide-react";
import { t, tc, tn } from "../i18n";
import { api, useApp, useChanged } from "../store";
import { confirmDialog } from "../ui/dialogs";

export interface Picture {
  id: string;
  name: string;
  ext: string;
  added: number;
  by: string;
  /** "" (a picture) or "overlay". */
  kind?: string;
}
interface Pictures {
  images: Picture[];
  /** Each source's slate. */
  sources: Record<string, string>;
}

export const pictureUrl = (id: string) => `/api/slates/${id}/image`;

export function usePictures(): [Pictures | null, () => Promise<void>] {
  const [s, setS] = useState<Pictures | null>(null);
  const load = useCallback(async () => {
    try {
      setS(await api<Pictures>("GET", "/api/slates"));
    } catch {
      /* the next change */
    }
  }, []);
  useEffect(() => void load(), [load]);
  useChanged("slates", () => void load());
  return [s, load];
}

/** The default picture, drawn small: the source's name on a dark picture. */
export function DefaultSlate({ name }: { name: string }) {
  return (
    <span className="slate-default">
      <b>{name}</b>
      <span>{t("The live comes back in a moment")}</span>
    </span>
  );
}

/** A picture (or the default one) in a small 16:9 frame. */
export function PictureThumb({ id, name, overlay }: { id?: string | null; name: string; overlay?: boolean }) {
  return (
    <span className={`slate-thumb ${overlay ? "checker" : ""}`}>
      {id ? <img src={pictureUrl(id)} alt="" /> : overlay ? null : <DefaultSlate name={name} />}
    </span>
  );
}

/**
 * Over the whole page: the pictures of one kind, the one chosen ticked; drop to add, the bin to
 * delete. `fallback`: the tile meaning "none" (the default picture), if there is one; `children`:
 * the settings beside the grid.
 */
export function PictureModal({
  title,
  overlay,
  chosen,
  onChoose,
  fallback,
  children,
  onClose,
}: {
  title: string;
  overlay?: boolean;
  chosen: string | null;
  onChoose: (id: string | null) => Promise<void> | void;
  fallback?: ReactNode;
  children?: ReactNode;
  onClose: () => void;
}) {
  const [pics, reload] = usePictures();
  const toast = useApp((s) => s.toast);
  const [over, setOver] = useState(false);
  const [sending, setSending] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  const types = overlay ? /^image\/png$/ : /^image\/(png|jpeg|webp)$/;
  const list = (pics?.images ?? []).filter((x) => (x.kind === "overlay") === !!overlay);

  const send = async (files: FileList | File[]) => {
    const ok = [...files].filter((f) => types.test(f.type));
    if (ok.length < [...files].length) toast(t(overlay ? "an overlay is a PNG" : "the image is a PNG, a JPEG or a WebP"), "error");
    let last: string | null = null;
    for (const f of ok) {
      setSending((n) => n + 1);
      try {
        const name = f.name.replace(/\.[^.]+$/, "");
        const img = await api<Picture>("POST", `/api/slates?name=${encodeURIComponent(name)}${overlay ? "&kind=overlay" : ""}`, f);
        last = img.id;
      } catch (e) {
        toast(`${f.name} : ${t((e as Error).message)}`, "error");
      } finally {
        setSending((n) => n - 1);
      }
    }
    await reload();
    // One picture sent: it is the one chosen.
    if (ok.length === 1 && last) await onChoose(last);
  };

  const remove = async (img: Picture) => {
    const users = overlay ? 0 : Object.values(pics?.sources ?? {}).filter((x) => x === img.id).length;
    const sure = await confirmDialog({
      title: t("Delete the image “{name}”?", { name: img.name }),
      message: users ? tn(users, "A source uses it: it goes back to the default slate.", "{n} sources use it: they go back to the default slate.") : t("Deleted for every source."),
      confirm: t("Delete"),
      danger: true,
    });
    if (!sure) return;
    try {
      await api("DELETE", `/api/slates/${img.id}`);
      if (chosen === img.id) await onChoose(null);
      await reload();
    } catch (e) {
      toast(t((e as Error).message), "error");
    }
  };

  return createPortal(
    <div className="modal-back" onPointerDown={onClose}>
      <div className={`modal wide slate-modal ${children ? "with-side" : ""}`} onPointerDown={(e) => e.stopPropagation()}>
        <header>
          <h2>{title}</h2>
          <button type="button" className="icon-btn ghost" title={t("Close")} onClick={onClose}>
            <X size={16} />
          </button>
        </header>
        <div className="slate-body">
          <div className="slate-main">
            <div
              className={`slate-drop ${over ? "over" : ""}`}
              role="button"
              tabIndex={0}
              onClick={() => input.current?.click()}
              onKeyDown={(e) => (e.key === "Enter" || e.key === " ") && input.current?.click()}
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
              <b>{sending > 0 ? t("Sending…") : t("Drop images here, or click to choose")}</b>
              <span>{overlay ? t("Transparent PNG, 10 MB at most") : t("PNG, JPEG or WebP, 10 MB at most · 16:9 fits best")}</span>
              <input
                ref={input}
                type="file"
                accept={overlay ? "image/png" : "image/png,image/jpeg,image/webp"}
                multiple
                hidden
                onChange={(e) => {
                  if (e.target.files) void send(e.target.files);
                  e.target.value = "";
                }}
              />
            </div>
            <div className="slate-grid">
              {fallback !== undefined && (
                <button type="button" className={`slate-tile ${chosen === null ? "on" : ""}`} onClick={() => void onChoose(null)}>
                  <span className="slate-pic">{fallback}</span>
                  <span className="slate-name">{overlay ? tc("overlay", "None") : t("Default")}</span>
                  {chosen === null && (
                    <span className="slate-check">
                      <Check size={12} />
                    </span>
                  )}
                </button>
              )}
              {list.map((img) => (
                <div
                  key={img.id}
                  className={`slate-tile ${chosen === img.id ? "on" : ""}`}
                  role="button"
                  tabIndex={0}
                  onClick={() => void onChoose(img.id)}
                  onKeyDown={(e) => e.key === "Enter" && void onChoose(img.id)}
                >
                  <span className={`slate-pic ${overlay ? "checker" : ""}`}>
                    <img src={pictureUrl(img.id)} alt="" loading="lazy" />
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
          </div>
          {children && <div className="slate-side">{children}</div>}
        </div>
        <footer>
          <button type="button" className="btn" onClick={onClose}>
            {t("Close")}
          </button>
        </footer>
      </div>
    </div>,
    document.body,
  );
}

/** The source's slate (sent while it is lost): its own picture, or the default one. */
export function SlateModal({ recId, name, onClose }: { recId: string; name: string; onClose: () => void }) {
  const [pics, reload] = usePictures();
  const toast = useApp((s) => s.toast);
  const choose = async (image: string | null) => {
    try {
      await api("PUT", `/api/slates/source/${recId}`, { image });
      await reload();
    } catch (e) {
      toast(t((e as Error).message), "error");
    }
  };
  return (
    <PictureModal title={t("Slate of “{name}”", { name })} chosen={pics?.sources[recId] ?? null} onChoose={choose} fallback={<DefaultSlate name={name} />} onClose={onClose} />
  );
}
