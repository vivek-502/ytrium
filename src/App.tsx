import React, { Suspense, lazy, useEffect } from "react";
import { Navigate, Route, Routes, useNavigate } from "react-router-dom";
import AppShell from "./components/layout/AppShell";
import { ToastHost } from "./components/common/ToastHost";
import { MenuHost } from "./components/common/MenuHost";
import { DialogHost } from "./components/dialogs/DialogHost";
import QueuePanel from "./components/player/QueuePanel";
import FullPlayer from "./components/player/FullPlayer";
import { Spinner, EmptyState, Button } from "./components/common/UI";
import { useLibraryStore } from "./stores/libraryStore";
import { useSettingsStore } from "./stores/settingsStore";
import { usePlayerStore } from "./stores/playerStore";
import { useKeyboardShortcuts } from "./hooks/useKeyboardShortcuts";

/* Code-split every page: only the shell loads up front. */
const Home = lazy(() => import("./pages/Home"));
const Search = lazy(() => import("./pages/Search"));
const Library = lazy(() => import("./pages/Library"));
const Liked = lazy(() => import("./pages/Liked"));
const RecentlyPlayed = lazy(() => import("./pages/RecentlyPlayed"));
const Playlist = lazy(() => import("./pages/Playlist"));
const Album = lazy(() => import("./pages/Album"));
const Artist = lazy(() => import("./pages/Artist"));
const Settings = lazy(() => import("./pages/Settings"));

const NotFound = () => {
  const navigate = useNavigate();
  return (
    <div className="page">
      <EmptyState
        icon="search"
        title="Page not found"
        message="That page doesn't exist. Head back home and keep listening."
        action={
          <Button variant="accent" onClick={() => navigate("/")}>
            Go home
          </Button>
        }
      />
    </div>
  );
};

const PageFallback = () => (
  <div className="page" style={{ display: "grid", placeItems: "center", minHeight: "40vh" }}>
    <Spinner large label="Loading page" />
  </div>
);

const BootSplash = () => (
  <div className="page boot-splash" role="status" aria-label="Loading Ytrium">
    <span className="boot-splash__wordmark">
      Ytri<span>um</span>
    </span>
    <Spinner large label="Loading Ytrium" />
  </div>
);

/** The app: hydrates local data, then mounts the shell. */
const App: React.FC = () => {
  const hydrateSettings = useSettingsStore((s) => s.hydrate);
  const hydrateLibrary = useLibraryStore((s) => s.hydrate);
  const hydratePlayer = usePlayerStore((s) => s.hydrate);
  const [hydrated, setHydrated] = React.useState(false);
  const [bootError, setBootError] = React.useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        hydrateSettings();
        await Promise.all([
          Promise.resolve(hydrateLibrary()),
          Promise.resolve(hydratePlayer()),
        ]);
      } catch (err) {
        if (!cancelled) {
          setBootError(err instanceof Error ? err.message : "Could not load your library.");
        }
      } finally {
        if (!cancelled) setHydrated(true);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [hydrateSettings, hydrateLibrary, hydratePlayer]);

  useKeyboardShortcuts();

  if (!hydrated) return <BootSplash />;

  if (bootError) {
    return (
      <div className="page" style={{ display: "grid", placeItems: "center", minHeight: "60vh" }}>
        <EmptyState
          icon="alert"
          title="Could not start Ytrium"
          message={bootError}
          action={
            <Button variant="accent" onClick={() => window.location.reload()}>
              Reload
            </Button>
          }
        />
      </div>
    );
  }

  return (
    <>
      <a className="sr-only" href="#main-content">
        Skip to main content
      </a>
      <Routes>
        <Route element={<AppShell />}>
          <Route
            index
            element={
              <Suspense fallback={<PageFallback />}>
                <Home />
              </Suspense>
            }
          />
          <Route
            path="search"
            element={
              <Suspense fallback={<PageFallback />}>
                <Search />
              </Suspense>
            }
          />
          <Route
            path="library"
            element={
              <Suspense fallback={<PageFallback />}>
                <Library />
              </Suspense>
            }
          />
          <Route
            path="liked"
            element={
              <Suspense fallback={<PageFallback />}>
                <Liked />
              </Suspense>
            }
          />
          <Route
            path="recently-played"
            element={
              <Suspense fallback={<PageFallback />}>
                <RecentlyPlayed />
              </Suspense>
            }
          />
          <Route
            path="playlist/:playlistId"
            element={
              <Suspense fallback={<PageFallback />}>
                <Playlist />
              </Suspense>
            }
          />
          <Route
            path="album/:artist/:title"
            element={
              <Suspense fallback={<PageFallback />}>
                <Album />
              </Suspense>
            }
          />
          <Route
            path="artist/:artistName"
            element={
              <Suspense fallback={<PageFallback />}>
                <Artist />
              </Suspense>
            }
          />
          <Route
            path="settings"
            element={
              <Suspense fallback={<PageFallback />}>
                <Settings />
              </Suspense>
            }
          />
          <Route path="home" element={<Navigate to="/" replace />} />
          <Route path="*" element={<NotFound />} />
        </Route>
      </Routes>

      {/* Global overlays */}
      <QueuePanel />
      <FullPlayer />
      <MenuHost />
      <DialogHost />
      <ToastHost />
    </>
  );
};

export default App;
