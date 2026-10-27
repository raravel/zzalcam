<script lang="ts">
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { loadLanguage } from "./lib/i18n.svelte";
  import Settings from "./views/Settings.svelte";
  import ControlBar from "./views/ControlBar.svelte";
  import Toast from "./views/Toast.svelte";

  // One bundle serves every window; the window label decides the view.
  const label = getCurrentWindow().label;
  const ready = loadLanguage();
  listen("language-changed", loadLanguage);
</script>

{#await ready then}
  {#if label === "controlbar"}
    <ControlBar />
  {:else if label === "toast"}
    <Toast />
  {:else}
    <Settings />
  {/if}
{/await}
