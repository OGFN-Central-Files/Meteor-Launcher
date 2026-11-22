import { create } from "zustand";
import { persist } from "zustand/middleware";
import { ConfigState } from "../vite-env";
import { invoke } from "@tauri-apps/api/core";

export const useConfigStore = create<ConfigState>()(
  persist(
    (set) => ({
      minimizeOnLaunch: false,
      theme: "meteor",

      editOnRelease: false,
      disablePreEdits: false,
      resetOnRelease: false,
      alwaysOnTop: false,
      mobileBuilds: false,
      bubbleBuilds: false,

      reduceMotion: false,

      setReduceMotion: (value) => set({ reduceMotion: value }),

      setMobileBuilds: (value) => set({ mobileBuilds: value }),
      setBubbleBuilds: (value) => set({ bubbleBuilds: value }),

      setEditOnRelease: (value) => set({ editOnRelease: value }),
      setDisablePreEdits: (value) => set({ disablePreEdits: value }),
      setResetOnRelease: (value) => set({ resetOnRelease: value }),

      setMinimizeOnLaunch: (value) => set({ minimizeOnLaunch: value }),
      setAlwaysOnTop: async (value) => {
        await invoke("set_always_on_top", { alwaysOnTop: value });
        set({ alwaysOnTop: value });
      },

      toggleMinimizeOnLaunch: () =>
        set((state) => ({ minimizeOnLaunch: !state.minimizeOnLaunch })),

      setTheme: (theme) => set({ theme }),
    }),
    {
      name: "settings",
    },
  ),
);
