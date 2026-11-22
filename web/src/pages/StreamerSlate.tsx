/*
 * Streamer: the pictures of a source's lives — its image (put on air by the Image button), its
 * overlays (transparent PNG) and its slates (sent while it is lost). A gallery of one kind: the one
 * chosen ticked, a tile to add (a click to pick files, or files dropped on the gallery), the bin on
 * a tile to delete one (after a confirmation). Without an image or a slate of its own, a source has
 * its name on a dark picture.
 */
import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { Check, Plus, Trash2 } from "lucide-react";
import { t, tn } from "../i18n";
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

/** One kind of pictures: `chosen` ticked; `fallback`, the tile meaning "none", if there is one. */
export function Gallery({
  overlay,
  chosen,
  onChoose,
  fallback,
  disabled,
}: {
  overlay?: boolean;
  chosen: string | null;
  onChoose: (id: string | null) => Promise<void> | void;
  fallback?: { name: string; pic: ReactNode };
  disabled?: boolean;
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

  const tile = (id: string | null, name: string, pic: ReactNode, img?: Picture) => (
    <div
      key={id ?? "none"}
      className={`gal-tile ${chosen === id ? "on" : ""}`}
      role="button"
      tabIndex={0}
      aria-disabled={disabled}
      onClick={() => !disabled && void onChoose(id)}
      onKeyDown={(e) => e.key === "Enter" && !disabled && void onChoose(id)}
    >
      <span className={`gal-pic ${overlay && id ? "checker" : ""}`}>{pic}</span>
      <span className="gal-name" title={name}>
        {name}
      </span>
      {chosen === id && (
        <span className="gal-check">
          <Check size={11} />
        </span>
      )}
      {img && !disabled && (
        <button
          type="button"
          className="gal-del icon-btn ghost danger"
          data-tip={t("Delete")}
          aria-label={t("Delete")}
          onClick={(e) => {
            e.stopPropagation();
            void remove(img);
          }}
        >
          <Trash2 size={13} />
        </button>
      )}
    </div>
  );

  return (
    <div
      className={`gal ${over ? "over" : ""}`}
      onDragOver={(e) => {
        if (disabled) return;
        e.preventDefault();
        setOver(true);
      }}
      onDragLeave={() => setOver(false)}
      onDrop={(e) => {
        e.preventDefault();
        setOver(false);
        if (!disabled) void send(e.dataTransfer.files);
      }}
    >
      {fallback && tile(null, fallback.name, fallback.pic)}
      {list.map((img) => tile(img.id, img.name, <img src={pictureUrl(img.id)} alt="" loading="lazy" />, img))}
      {!disabled && (
        <button type="button" className="gal-add" onClick={() => input.current?.click()}>
          <Plus size={20} />
          <span>{sending > 0 ? t("Sending…") : overlay ? t("Add a PNG") : t("Add an image")}</span>
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
        </button>
      )}
    </div>
  );
}
