<script lang="ts">
  /**
   * Whether a volume is mounted right now, in the glyphs the CLI prints —
   * filled for online, hollow for offline.
   *
   * Two small circles say nothing to anyone who cannot see them, and a
   * screen reader left to itself announces "Card black circle", so the badge
   * carries its own name: `role="img"` makes the glyph one graphic and
   * `aria-label` says in words what it means.
   *
   * `label` is empty on the volumes surface, where the volume's label
   * already has a column of its own and the badge is only the state.
   */
  let { label = "", online }: { label?: string; online: boolean } = $props();

  let state = $derived(online ? "online" : "offline");
</script>

<span
  class="badge"
  role="img"
  aria-label={label === "" ? state : `${label} ${state}`}
  >{label}{online ? "●" : "○"}</span
>
