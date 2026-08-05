<script lang="ts">
  /**
   * The degradation notices an outcome carries, verbatim and permanent —
   * nothing here dismisses, shortens or deduplicates them.
   *
   * THE KEYING RULE, stated once here for the whole app: key an `{#each}`
   * ONLY where the Rust source is a map or set keyed by that value, so the
   * key is unique by construction. Everything else is unkeyed.
   *
   * Notices are the reason the rule exists. They arrive as a `Vec<String>`
   * drained from the notices sink, and one outcome can legitimately carry
   * the same line twice — a saved-search run drains the same corrupt-log
   * warning from the projection load and from the catalog open, byte for
   * byte. A keyed each throws on the repeat and takes the whole surface
   * down with it, so this one is unkeyed and both copies render.
   * `Notices.test.ts` pins that.
   */
  let { notices }: { notices: string[] | undefined } = $props();
</script>

{#each notices ?? [] as notice}
  <p class="notice">{notice}</p>
{/each}
