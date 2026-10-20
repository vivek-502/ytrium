import React, { useMemo, useRef, useState } from "react";
import { useSettingsStore } from "../stores/settingsStore";
import { useLibraryStore } from "../stores/libraryStore";
import { usePlayerStore } from "../stores/playerStore";
import { ui } from "../stores/uiStore";
import { confirmDialog } from "../components/common/Overlay";
import { Button, Slider, cx } from "../components/common/UI";
import { flushSaves } from "../lib/storage";
import { applyImport, buildExportPayload, downloadExport, parseImportFile } from "../lib/exportImport";
import { pluralize } from "../lib/format";
import type { AccentKey, AudioQuality, Settings as SettingsType, Theme } from "../types";

/* ------------------------------------------------------------------ atoms */

const Switch: React.FC<{
  checked: boolean;
  onChange: (next: boolean) => void;
  label: string;
}> = ({ checked, onChange, label }) => (
  <button
    type="button"
    role="switch"
    aria-checked={checked}
    aria-label={label}
    className="switch"
    onClick={() => onChange(!checked)}
  />
);

const Segmented: React.FC<{
  value: string;
  options: Array<{ value: string; label: string }>;
  onChange: (value: string) => void;
  ariaLabel: string;
}> = ({ value, options, onChange, ariaLabel }) => (
  <div className="seg" role="radiogroup" aria-label={ariaLabel}>
    {options.map((opt) => (
      <button
        key={opt.value}
        type="button"
        role="radio"
        aria-checked={value === opt.value}
        className={cx(value === opt.value && "is-active")}
        onClick={() => onChange(opt.value)}
      >
        {opt.label}
      </button>
    ))}
  </div>
);

const SettingRow: React.FC<{
  title: string;
  desc?: string;
  control: React.ReactNode;
}> = ({ title, desc, control }) => (
  <div className="setting-row">
    <div style={{ minWidth: 0 }}>
      <div className="setting-row__title">{title}</div>
      {desc && <div className="setting-row__desc">{desc}</div>}
    </div>
    {control}
  </div>
);

/* --------------------------------------------------------- accent swatches */

const ACCENTS: Array<{ key: AccentKey; label: string; color: string }> = [
  { key: "purple", label: "Purple", color: "#757de8" },
  { key: "indigo", label: "Indigo", color: "#7986cb" },
  { key: "blue", label: "Blue", color: "#64b5f6" },
  { key: "teal", label: "Teal", color: "#4db6ac" },
  { key: "green", label: "Green", color: "#81c784" },
  { key: "pink", label: "Pink", color: "#f06292" },
];

/* Mirrors the settings page heading copy. */

/* ------------------------------------------------------------------ page */

const Settings: React.FC = () => {
  const settings = useSettingsStore((s) => s.settings);
  const update = useSettingsStore((s) => s.update);
  const resetSettings = useSettingsStore((s) => s.reset);

  const playlists = useLibraryStore((s) => s.playlists);
  const liked = useLibraryStore((s) => s.liked);
  const history = useLibraryStore((s) => s.history);
  const searchHistory = useLibraryStore((s) => s.searchHistory);
  const catalog = useLibraryStore((s) => s.catalog);
  const clearListeningData = useLibraryStore((s) => s.clearListeningData);
  const clearSearchHistory = useLibraryStore((s) => s.clearSearchHistory);

  const currentTrack = usePlayerStore((s) => s.currentTrack);
  const [exporting, setExporting] = useState(false);
  const [importing, setImporting] = useState(false);
  const importRef = useRef<HTMLInputElement>(null);

  const patch = (p: Partial<SettingsType>) => update(p);

  const stats = useMemo(
    () => ({
      playlists: playlists.length,
      liked: Object.keys(liked).length,
      history: history.length,
      searches: searchHistory.length,
      catalog: Object.keys(catalog).length,
    }),
    [playlists, liked, history, searchHistory, catalog]
  );

  /* ------------------------------------------------------------ data ops */

  const exportData = async () => {
    setExporting(true);
    try {
      const payload = await buildExportPayload();
      const saved = await downloadExport(payload);
      if (saved) ui.toast("Library exported as JSON", "success");
    } catch (err) {
      ui.toast(
        err instanceof Error && err.name === "ImportError"
          ? err.message
          : "Export failed - your data was not changed",
        "error"
      );
    } finally {
      setExporting(false);
    }
  };

  const handleImportFile = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    e.target.value = "";
    if (!file) return;
    setImporting(true);
    try {
      const payload = await parseImportFile(file);
      const summary = await applyImport(payload);
      const bits: string[] = [];
      if (summary.playlists) bits.push(pluralize(summary.playlists, "playlist"));
      if (summary.liked) bits.push(pluralize(summary.liked, "liked song"));
      if (summary.history) bits.push(pluralize(summary.history, "history entry"));
      if (summary.settings) bits.push("settings");
      if (bits.length === 0) {
        ui.toast("Nothing new was found in that export", "info");
        return;
      }
      ui.toast(`Imported ${bits.join(", ")}`, "success");
    } catch (err) {
      ui.toast(
        err instanceof Error && err.name === "ImportError"
          ? err.message
          : "Import failed - nothing was changed",
        "error"
      );
    } finally {
      setImporting(false);
    }
  };

  const wipeListeningData = async () => {
    const ok = await confirmDialog({
      title: "Clear listening data?",
      message:
        "History, search history and recommendation signals will be deleted. Playlists, liked songs and settings stay where they are.",
      confirmLabel: "Clear data",
      danger: true,
    });
    if (ok) clearListeningData();
  };

  const wipeSearchHistory = async () => {
    const ok = await confirmDialog({
      title: "Clear search history?",
      message: "Your recent search terms will be removed from this device.",
      confirmLabel: "Clear searches",
      danger: true,
    });
    if (ok) {
      clearSearchHistory();
      ui.toast("Search history cleared", "success");
    }
  };

  const resetEverything = async () => {
    const ok = await confirmDialog({
      title: "Reset Ytrium?",
      message:
        "This deletes EVERYTHING stored on this device - playlists, liked songs, history, settings and the player queue - and reloads the app. It cannot be undone. Export your library first if you want a backup.",
      confirmLabel: "Reset everything",
      danger: true,
    });
    if (!ok) return;
    ui.toast("Resetting Ytrium…", "info");
    flushSaves();
    await useLibraryStore.getState().resetApplication();
  };

  const restoreDefaults = async () => {
    const ok = await confirmDialog({
      title: "Restore default settings?",
      message: "Playback and appearance preferences go back to their defaults. Your library is untouched.",
      confirmLabel: "Restore defaults",
    });
    if (ok) {
      resetSettings();
      ui.toast("Settings restored to defaults", "success");
    }
  };

  return (
    <div className="page" style={{ maxWidth: 860 }}>
      <header style={{ padding: "18px 0 6px" }}>
        <h1 style={{ fontSize: "clamp(26px, 4vw, 36px)", fontWeight: 900, letterSpacing: "-0.03em" }}>
          Settings
        </h1>
        <p className="text-muted" style={{ fontSize: 13.5, marginTop: 4 }}>
          Everything here stays on this device - no syncing, no account, nothing leaves your
          computer except music requests to the provider.
        </p>
      </header>

      {/* --------------------------------------------------------- playback */}
      <section className="settings-group" aria-labelledby="set-playback">
        <h2 className="settings-group__title" id="set-playback">
          Playback
        </h2>
        <p className="settings-group__hint">How the player behaves while music is running.</p>

        <SettingRow
          title="Autoplay"
          desc="When the queue runs out, keep the music going with similar tracks."
          control={
            <Switch
              label="Autoplay"
              checked={settings.autoplay}
              onChange={(v) => patch({ autoplay: v })}
            />
          }
        />

        <SettingRow
          title="Crossfade"
          desc={
            settings.crossfadeSeconds > 0
              ? `Fade ${settings.crossfadeSeconds}s into the next track.`
              : "Off - tracks play back to back with a hard cut."
          }
          control={
            <div className="range-row">
              <Slider
                value={settings.crossfadeSeconds / 12}
                label="Crossfade duration"
                ariaValueText={(v) =>
                  Math.round(v * 12) === 0 ? "Off" : `${Math.round(v * 12)} seconds`
                }
                onChange={(v) => patch({ crossfadeSeconds: Math.round(v * 12) })}
              />
              <span className="text-muted" style={{ fontSize: 13, minWidth: 52, textAlign: "right" }}>
                {settings.crossfadeSeconds === 0 ? "Off" : `${settings.crossfadeSeconds}s`}
              </span>
            </div>
          }
        />

        <SettingRow
          title="Audio quality"
          desc="High streams a clearer source when the provider offers one; low starts faster on slow connections."
          control={
            <Segmented
              ariaLabel="Audio quality"
              value={settings.audioQuality}
              options={[
                { value: "auto", label: "Auto" },
                { value: "high", label: "High" },
                { value: "low", label: "Low" },
              ]}
              onChange={(v) => patch({ audioQuality: v as AudioQuality })}
            />
          }
        />

        <SettingRow
          title="Volume normalization"
          desc="Even out loud and quiet tracks so you are not reaching for the volume."
          control={
            <Switch
              label="Volume normalization"
              checked={settings.normalizeVolume}
              onChange={(v) => patch({ normalizeVolume: v })}
            />
          }
        />
      </section>

      {/* ------------------------------------------------------- appearance */}
      <section className="settings-group" aria-labelledby="set-appearance">
        <h2 className="settings-group__title" id="set-appearance">
          Appearance
        </h2>
        <p className="settings-group__hint">Theme, accent and how dense the interface feels.</p>

        <SettingRow
          title="Theme"
          desc="Dark is kinder at night; light holds up in sunlight."
          control={
            <Segmented
              ariaLabel="Theme"
              value={settings.theme}
              options={[
                { value: "dark", label: "Dark" },
                { value: "light", label: "Light" },
              ]}
              onChange={(v) => patch({ theme: v as Theme })}
            />
          }
        />

        <SettingRow
          title="Accent color"
          desc="The highlight color used across buttons, sliders and the player."
          control={
            <div className="swatches" role="radiogroup" aria-label="Accent color">
              {ACCENTS.map((accent) => (
                <button
                  key={accent.key}
                  type="button"
                  role="radio"
                  aria-checked={settings.accent === accent.key}
                  aria-label={accent.label}
                  title={accent.label}
                  className={cx("swatch", settings.accent === accent.key && "is-active")}
                  style={{ background: accent.color }}
                  onClick={() => patch({ accent: accent.key })}
                />
              ))}
            </div>
          }
        />

        <SettingRow
          title="Compact mode"
          desc="Tighter rows and smaller gaps - more on screen at once."
          control={
            <Switch
              label="Compact mode"
              checked={settings.compactMode}
              onChange={(v) => patch({ compactMode: v })}
            />
          }
        />

        <SettingRow
          title="Reduce motion"
          desc="Turn off transitions and animated effects throughout the app."
          control={
            <Switch
              label="Reduce motion"
              checked={settings.reduceMotion}
              onChange={(v) => patch({ reduceMotion: v })}
            />
          }
        />
      </section>

      {/* --------------------------------------------------------- content */}
      <section className="settings-group" aria-labelledby="set-content">
        <h2 className="settings-group__title" id="set-content">
          Content
        </h2>
        <p className="settings-group__hint">What shows up when you search.</p>

        <SettingRow
          title="Official playlists only"
          desc="Search playlists normally come from anyone on YouTube. Turn this on to keep only playlists published by the artist, label or an official channel."
          control={
            <Switch
              label="Official playlists only"
              checked={settings.officialPlaylistsOnly}
              onChange={(v) => patch({ officialPlaylistsOnly: v })}
            />
          }
        />
      </section>

      {/* ------------------------------------------------------------ data */}
      <section className="settings-group" aria-labelledby="set-data">
        <h2 className="settings-group__title" id="set-data">
          Your data
        </h2>
        <p className="settings-group__hint">
          {pluralize(stats.playlists, "playlist")} · {pluralize(stats.liked, "liked song")} ·{" "}
          {pluralize(stats.history, "history entry")} · {pluralize(stats.searches, "recent search")} ·{" "}
          {pluralize(stats.catalog, "known track")} stored locally.
        </p>

        <SettingRow
          title="Export library"
          desc="Download playlists, likes, history and settings as a JSON file."
          control={
            <Button variant="soft" icon="copy" onClick={() => void exportData()} disabled={exporting}>
              {exporting ? "Exporting…" : "Export JSON"}
            </Button>
          }
        />

        <SettingRow
          title="Import library"
          desc="Merge playlists, likes, history and settings from an export file. Nothing on this device is overwritten or removed."
          control={
            <>
              <input
                ref={importRef}
                type="file"
                accept=".json,application/json"
                aria-hidden="true"
                tabIndex={-1}
                style={{ display: "none" }}
                onChange={(e) => void handleImportFile(e)}
              />
              <Button
                variant="soft"
                icon="plus"
                onClick={() => importRef.current?.click()}
                disabled={importing}
              >
                {importing ? "Importing…" : "Import JSON"}
              </Button>
            </>
          }
        />

        <SettingRow
          title="Clear listening data"
          desc="Erase history, recent searches and the signals behind your recommendations."
          control={
            <Button variant="ghost" icon="trash" onClick={() => void wipeListeningData()}>
              Clear data
            </Button>
          }
        />

        <SettingRow
          title="Clear search history"
          desc="Remove the recent search terms shown while you type."
          control={
            <Button
              variant="ghost"
              icon="x"
              onClick={() => void wipeSearchHistory()}
              disabled={!stats.searches}
            >
              Clear searches
            </Button>
          }
        />

        <SettingRow
          title="Reset Ytrium"
          desc="Delete everything on this device and start over. This cannot be undone."
          control={
            <Button variant="danger" icon="warning" onClick={() => void resetEverything()}>
              Reset app
            </Button>
          }
        />
      </section>

      {/* --------------------------------------------------------- shortcuts */}
      <section className="settings-group" aria-labelledby="set-about">
        <h2 className="settings-group__title" id="set-about">
          About
        </h2>
        <p className="settings-group__hint">
          Ytrium is a local-first desktop music player. Search, metadata and playback all run
          locally through the bundled engine - nothing about you is uploaded anywhere.
        </p>

        <SettingRow
          title="Keyboard shortcuts"
          desc="Control playback without touching the mouse."
          control={
            <Button variant="ghost" icon="keyboard" onClick={() => ui.openDialog({ type: "shortcuts" })}>
              View shortcuts
            </Button>
          }
        />

        <SettingRow
          title="Restore defaults"
          desc={
            currentTrack
              ? `Now playing: ${currentTrack.title} - the queue is kept.`
              : "Reset playback and appearance preferences only."
          }
          control={
            <Button variant="ghost" icon="refresh" onClick={() => void restoreDefaults()}>
              Restore
            </Button>
          }
        />
      </section>
    </div>
  );
};

export default Settings;
