<script lang="ts" generics="T">
  import { tick } from "svelte";
  import { ChevronDown } from "@lucide/svelte";

  interface Option {
    value: T;
    label: string;
  }

  // Values are compared with ===, so use primitives (numbers, strings).
  let {
    value = $bindable(),
    options,
    label = "",
    wide = false,
  }: { value: T; options: Option[]; label?: string; wide?: boolean } = $props();

  const GAP = 4;
  const MARGIN = 8;
  const uid = $props.id();

  let button = $state<HTMLButtonElement | null>(null);
  let list = $state<HTMLUListElement | null>(null);
  let open = $state(false);
  let placed = $state(false);
  let up = $state(false);
  let active = $state(0);
  let pos = $state({ left: 0, top: 0, width: 0, maxHeight: 0 });
  const current = $derived(options.find((o) => o.value === value));

  // The list is position: fixed so scrolling containers never clip it. It is measured once
  // rendered, then kept inside the window: below the trigger, or above when that has more room.
  async function show() {
    if (!button || !options.length) return;
    active = Math.max(0, options.findIndex((o) => o.value === value));
    placed = false;
    open = true;
    await tick();
    if (!list || !button) return;
    const r = button.getBoundingClientRect();
    const below = innerHeight - r.bottom - GAP - MARGIN;
    const above = r.top - GAP - MARGIN;
    const h = list.scrollHeight;
    up = h > below && above > below;
    const room = up ? above : below;
    pos = {
      left: Math.max(MARGIN, Math.min(r.left, innerWidth - list.offsetWidth - MARGIN)),
      top: up ? r.top - GAP - Math.min(h, room) : r.bottom + GAP,
      width: r.width,
      maxHeight: room,
    };
    placed = true;
    await tick();
    list.children[active]?.scrollIntoView({ block: "nearest" });
  }

  function close(refocus = true) {
    open = false;
    if (refocus) button?.focus();
  }

  function choose(i: number) {
    if (!options[i]) return;
    value = options[i].value;
    close();
  }

  /** The next option after `from` whose label starts with `key`, or -1. */
  function match(key: string, from: number) {
    const k = key.toLowerCase();
    for (let step = 1; step <= options.length; step++) {
      const i = (from + step) % options.length;
      if (options[i].label.toLowerCase().startsWith(k)) return i;
    }
    return -1;
  }

  function onkeydown(e: KeyboardEvent) {
    const typed = e.key.length === 1 && e.key !== " " && !e.ctrlKey && !e.metaKey && !e.altKey;
    let handled = true;
    if (!open) {
      if (["ArrowDown", "ArrowUp", "Enter", " "].includes(e.key)) show();
      else if (typed) {
        // Like a native select: typing picks the next matching option without opening.
        const i = match(e.key, Math.max(0, options.findIndex((o) => o.value === value)));
        if (i >= 0) value = options[i].value;
      } else handled = false;
    } else if (e.key === "ArrowDown") active = Math.min(options.length - 1, active + 1);
    else if (e.key === "ArrowUp") active = Math.max(0, active - 1);
    else if (e.key === "Home") active = 0;
    else if (e.key === "End") active = options.length - 1;
    else if (e.key === "Enter" || e.key === " ") choose(active);
    else if (e.key === "Escape") close();
    else if (e.key === "Tab") (close(false), (handled = false));
    else if (typed) {
      const i = match(e.key, active);
      if (i >= 0) active = i;
    } else if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") handled = false;
    if (open && handled) list?.children[active]?.scrollIntoView({ block: "nearest" });
    // Keep keys away from window-level shortcuts (e.g. the editor's Space and arrow keys).
    if (handled) {
      e.preventDefault();
      e.stopPropagation();
    }
  }

  function onWindowPointerDown(e: PointerEvent) {
    const target = e.target as Node;
    if (open && !button?.contains(target) && !list?.contains(target)) close(false);
  }

  // Scrolling anything that carries the trigger would leave the fixed list behind, so it closes.
  // Other scrolls (e.g. the editor's frame strip during playback) are ignored.
  $effect(() => {
    if (!open) return;
    const onScroll = (e: Event) => {
      const target = e.target as Node;
      if (target === document || (button && target.contains(button))) close(false);
    };
    addEventListener("scroll", onScroll, true);
    return () => removeEventListener("scroll", onScroll, true);
  });
</script>

<svelte:window
  onpointerdown={onWindowPointerDown}
  onresize={() => open && close(false)}
  onblur={() => open && close(false)}
/>

<button
  bind:this={button}
  type="button"
  role="combobox"
  class="select"
  class:wide
  class:open
  aria-haspopup="listbox"
  aria-expanded={open}
  aria-controls="{uid}-list"
  aria-activedescendant={open ? `${uid}-opt-${active}` : undefined}
  aria-labelledby="{uid}-label {uid}-value"
  onclick={() => (open ? close() : show())}
  {onkeydown}
>
  <span id="{uid}-label" hidden>{label}</span>
  <!-- Every label sits in the same grid cell, so the trigger is as wide as the longest one. -->
  <span class="values">
    <span id="{uid}-value" class="value">{current?.label ?? String(value ?? "")}</span>
    {#each options as o}<span class="value ghost" aria-hidden="true">{o.label}</span>{/each}
  </span>
  <ChevronDown size={14} />
</button>

{#if open}
  <ul
    bind:this={list}
    id="{uid}-list"
    class="select-list"
    class:up
    class:placed
    role="listbox"
    aria-label={label || undefined}
    style:left="{pos.left}px"
    style:top="{pos.top}px"
    style:min-width="{pos.width}px"
    style:max-height={placed ? `${pos.maxHeight}px` : undefined}
  >
    {#each options as o, i}
      <!-- svelte-ignore a11y_click_events_have_key_events -->
      <li
        id="{uid}-opt-{i}"
        role="option"
        aria-selected={o.value === value}
        class:selected={o.value === value}
        class:active={i === active}
        onpointerenter={() => (active = i)}
        onclick={() => choose(i)}
      >
        {o.label}
      </li>
    {/each}
  </ul>
{/if}

<style>
  .select {
    height: 32px;
    min-width: 72px;
    padding: 0 10px 0 11px;
    display: inline-flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    border: 1px solid var(--control-border);
    border-bottom-color: var(--control-border-strong);
    border-radius: 6px;
    background: var(--control);
    text-align: left;
    transition: background 0.1s;
  }
  .select:hover,
  .select.open {
    background: var(--control-hover);
  }
  .select :global(svg) {
    flex: none;
    color: var(--muted);
  }
  .select.wide {
    width: 100%;
  }
  .values {
    display: grid;
    min-width: 0;
  }
  .value {
    grid-area: 1 / 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .ghost {
    visibility: hidden;
  }
  .select-list {
    position: fixed;
    z-index: 100;
    margin: 0;
    padding: 4px;
    overflow-y: auto;
    visibility: hidden;
    list-style: none;
    border: 1px solid var(--flyout-border);
    border-radius: 8px;
    background: var(--flyout);
    color: var(--text);
    box-shadow: 0 8px 16px rgba(0, 0, 0, 0.14), 0 0 2px rgba(0, 0, 0, 0.1);
  }
  .select-list.placed {
    visibility: visible;
    animation: drop 0.12s ease-out;
  }
  .select-list.up.placed {
    animation-name: rise;
  }
  li {
    position: relative;
    flex: none;
    height: 32px;
    padding: 0 28px 0 12px;
    display: flex;
    align-items: center;
    border-radius: 4px;
    white-space: nowrap;
    cursor: pointer;
  }
  li.active {
    background: var(--item-hover);
  }
  li.selected {
    background: var(--item-selected);
  }
  /* Windows 11 marks the chosen item with a short accent pill. */
  li.selected::before {
    content: "";
    position: absolute;
    left: 0;
    top: 50%;
    width: 3px;
    height: 16px;
    border-radius: 2px;
    background: var(--accent);
    transform: translateY(-50%);
  }
  li.selected.active {
    background: var(--item-hover);
  }
  @keyframes drop {
    from {
      opacity: 0;
      transform: translateY(-4px);
    }
  }
  @keyframes rise {
    from {
      opacity: 0;
      transform: translateY(4px);
    }
  }
</style>
