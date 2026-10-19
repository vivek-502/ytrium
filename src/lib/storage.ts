import { idb } from './idb';

/**
 * Storage abstraction.
 *
 * All persistence flows through this module (`storage.get/save/update/
 * delete/clear`). Components never touch IndexedDB or localStorage directly.
 *
 * Ytrium is single-user and local-first: there is no account and no scoping -
 * one library, one history, one queue, one set of settings per machine.
 */
export const LIBRARY_KEY = 'library.v1';
export const HISTORY_KEY = 'history.v1';
export const PLAYER_KEY = 'player.v1';
export const SETTINGS_KEY = 'ytrium-web.settings.v1';
/** localStorage settings keys written by the web build; read as fallbacks. */
const LEGACY_SETTINGS_BASES = ['yttrium.settings.v1', 'sonare.settings.v1'];

/**
 * Copies settings left behind by an earlier name onto the current key the
 * first time the app runs. Only acts while the current key is still missing.
 */
function migrateLegacySettings(): void {
  try {
    if (localStorage.getItem(SETTINGS_KEY) !== null) return;
    for (const base of LEGACY_SETTINGS_BASES) {
      const legacy = localStorage.getItem(base);
      if (legacy !== null) {
        localStorage.setItem(SETTINGS_KEY, legacy);
        return;
      }
    }
  } catch (err) {
    console.error('[storage] legacy settings migration failed', err);
  }
}

migrateLegacySettings();

export const storage = {
  async get<T>(key: string): Promise<T | undefined> {
    try {
      return await idb.get<T>(key);
    } catch (err) {
      console.error('[storage] get failed', key, err);
      return undefined;
    }
  },

  async save<T>(key: string, value: T): Promise<void> {
    try {
      await idb.set(key, value);
    } catch (err) {
      console.error('[storage] save failed', key, err);
    }
  },

  async update<T>(key: string, updater: (prev: T | undefined) => T): Promise<T | undefined> {
    try {
      const next = updater(await idb.get<T>(key));
      await idb.set(key, next);
      return next;
    } catch (err) {
      console.error('[storage] update failed', key, err);
      return undefined;
    }
  },

  async delete(key: string): Promise<void> {
    try {
      await idb.delete(key);
    } catch (err) {
      console.error('[storage] delete failed', key, err);
    }
  },

  /** Clears every stored key plus legacy settings leftovers. */
  async clear(): Promise<void> {
    try {
      const keys = await idb.keys();
      for (const k of keys) {
        if (String(k) === '__migrated_from_legacy_db') continue;
        await idb.delete(String(k));
      }
      localStorage.removeItem(SETTINGS_KEY);
      for (const base of LEGACY_SETTINGS_BASES) {
        localStorage.removeItem(base);
      }
    } catch (err) {
      console.error('[storage] clear failed', err);
    }
  },
};

/* -------- Small preferences: localStorage only (tiny, synchronous). ------ */

export const prefStorage = {
  get<T>(key: string, fallback: T): T {
    try {
      const raw = localStorage.getItem(key);
      if (raw === null) return fallback;
      return JSON.parse(raw) as T;
    } catch {
      return fallback;
    }
  },
  set<T>(key: string, value: T): void {
    try {
      localStorage.setItem(key, JSON.stringify(value));
    } catch {
      // Quota / private mode - preferences simply do not persist.
    }
  },
  remove(key: string): void {
    try {
      localStorage.removeItem(key);
    } catch {
      /* noop */
    }
  },
};

/* ------------------- Debounced persistence helpers ----------------------- */

const pending = new Map<string, { timer: ReturnType<typeof setTimeout>; write: () => void }>();

/** Coalesces rapid state changes into one IndexedDB write per key. */
export function saveDebounced<T>(key: string, value: T, delay = 600): void {
  const existing = pending.get(key);
  if (existing) clearTimeout(existing.timer);
  const timer = setTimeout(() => {
    pending.delete(key);
    void storage.save(key, value);
  }, delay);
  pending.set(key, { timer, write: () => void storage.save(key, value) });
}

/** Flushes any pending debounced writes immediately. */
export function flushSaves(): void {
  for (const [, entry] of pending) {
    clearTimeout(entry.timer);
    entry.write();
  }
  pending.clear();
}
