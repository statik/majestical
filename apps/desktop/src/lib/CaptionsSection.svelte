<script lang="ts">
  // The Captions section of Settings: which describer writes captions and
  // suggests tags — backend, base URL, model, and OpenRouter's key — through
  // `api-captions.ts`'s four commands. All of its words live in
  // `captions-status.ts`; this file is the form and its wiring.
  //
  // The key is write-only from here: the field starts empty on every load
  // and is emptied again after a successful Save, and nothing the section
  // renders is ever the key itself — only `key_source`'s status line. A
  // failed Save keeps the typed key so the operator can retry.
  //
  // Test probes what is STORED, not what is typed, so it is disabled while
  // the form differs from the saved describer.
  import { errorMessage, errorNotices } from "./api";
  import type {
    DescriberBackend,
    DescriberProbeOutcome,
    DescriberSettingsOutcome,
    SaveDescriberReq,
  } from "./api-captions";
  import { captionsApi } from "./api-captions";
  import {
    BACKENDS,
    keyPlaceholder,
    keyStatusLine,
    removeKeyVisible,
    SAVED_LINE,
    testLines,
    UNCONFIGURED_LINE,
  } from "./captions-status";
  import Notices from "./Notices.svelte";

  let { onchanged = () => {} }: { onchanged?: () => void } = $props();

  let outcome = $state<DescriberSettingsOutcome | null>(null);
  let backend = $state<DescriberBackend>("ollama");
  let baseUrl = $state(defaultUrl("ollama"));
  let model = $state("");
  let apiKey = $state("");
  let saved = $state(false);
  let probe = $state<DescriberProbeOutcome | null>(null);
  let error = $state<string | null>(null);
  let failureNotices = $state<string[]>([]);
  /** One command in flight at a time: Save, Test and Remove key all
   *  disable while any of them runs. */
  let busy = $state(false);

  const describer = $derived(outcome?.describer ?? null);
  const keySource = $derived(describer?.key_source ?? "none");
  const dirty = $derived(
    describer === null ||
      backend !== describer.backend ||
      baseUrl.trim() !== describer.base_url ||
      model.trim() !== describer.model ||
      apiKey.trim() !== "",
  );

  $effect(() => {
    void load();
  });

  function defaultUrl(value: DescriberBackend): string {
    return BACKENDS.find((entry) => entry.value === value)?.baseUrl ?? "";
  }

  function fill(next: DescriberSettingsOutcome) {
    outcome = next;
    backend = next.describer?.backend ?? "ollama";
    baseUrl = next.describer?.base_url ?? defaultUrl("ollama");
    model = next.describer?.model ?? "";
  }

  function fail(failure: unknown) {
    error = errorMessage(failure);
    failureNotices = errorNotices(failure);
  }

  function succeed() {
    error = null;
    failureNotices = [];
  }

  function edited() {
    saved = false;
  }

  /** A URL still holding some backend's default follows the backend; one
   *  the operator typed is theirs and stays. A typed key is dropped when the
   *  key field goes away: no secret is held that the operator cannot see. */
  function changeBackend(next: DescriberBackend) {
    const untouched =
      baseUrl.trim() === "" || BACKENDS.some((entry) => entry.baseUrl === baseUrl.trim());
    backend = next;
    if (untouched) baseUrl = defaultUrl(next);
    if (next !== "open-router") apiKey = "";
    edited();
  }

  async function load() {
    try {
      fill(await captionsApi.describerSettings());
      succeed();
    } catch (failure) {
      fail(failure);
    }
  }

  function saveRequest(): SaveDescriberReq {
    const req: SaveDescriberReq = { backend, model: model.trim() };
    if (baseUrl.trim() !== "") req.base_url = baseUrl.trim();
    if (backend === "open-router" && apiKey.trim() !== "") req.api_key = apiKey;
    return req;
  }

  async function save() {
    if (busy) return;
    busy = true;
    try {
      fill(await captionsApi.saveDescriber(saveRequest()));
      apiKey = "";
      saved = true;
      probe = null;
      succeed();
      onchanged();
    } catch (failure) {
      fail(failure);
    } finally {
      busy = false;
    }
  }

  async function test() {
    if (busy) return;
    busy = true;
    probe = null;
    try {
      probe = await captionsApi.testDescriber();
      succeed();
    } catch (failure) {
      fail(failure);
    } finally {
      busy = false;
    }
  }

  async function removeKey() {
    if (busy) return;
    busy = true;
    try {
      outcome = await captionsApi.clearDescriberKey();
      succeed();
      onchanged();
    } catch (failure) {
      fail(failure);
    } finally {
      busy = false;
    }
  }
</script>

<section class="settings-section">
  <div class="settings-section-head">
    <div>
      <h3>Captions</h3>
      <p class="settings-section-sub">
        The service that writes captions and suggests tags. Ollama and LM Studio run on this
        Mac; OpenRouter is a hosted API.
      </p>
    </div>
  </div>

  <Notices notices={outcome?.notices ?? []} />
  {#if outcome !== null && describer === null}
    <p class="notice">{UNCONFIGURED_LINE}</p>
  {/if}

  <div class="captions-form">
    <label for="captions-backend">Backend</label>
    <select
      id="captions-backend"
      data-e2e="captions-backend"
      value={backend}
      onchange={(event) => changeBackend(event.currentTarget.value as DescriberBackend)}
    >
      {#each BACKENDS as { value, label }}
        <option {value}>{label}</option>
      {/each}
    </select>
    <label for="captions-base-url">Base URL</label>
    <input id="captions-base-url" bind:value={baseUrl} oninput={edited} />
    <label for="captions-model">Model</label>
    <input
      id="captions-model"
      data-e2e="captions-model"
      placeholder="The model's name as the backend lists it"
      bind:value={model}
      oninput={edited}
    />
    {#if backend === "open-router"}
      <label for="captions-key">API key</label>
      <input
        id="captions-key"
        type="password"
        autocomplete="off"
        placeholder={keyPlaceholder(keySource)}
        bind:value={apiKey}
        oninput={edited}
      />
      <p class="captions-key-status">{keyStatusLine(keySource)}</p>
    {/if}
  </div>

  <div class="captions-actions">
    <button
      type="button"
      class="ctl-btn"
      data-e2e="captions-save"
      disabled={busy || model.trim() === ""}
      onclick={() => void save()}>Save</button
    >
    <button
      type="button"
      class="ctl-btn"
      disabled={busy || dirty}
      title={describer !== null && dirty ? "Save before testing" : undefined}
      onclick={() => void test()}>Test</button
    >
    {#if saved}
      <span class="captions-saved" data-e2e="captions-saved">{SAVED_LINE}</span>
    {/if}
    {#if backend === "open-router" && removeKeyVisible(keySource)}
      <span class="spacer"></span>
      <button
        type="button"
        class="ctl-btn"
        disabled={busy}
        onclick={() => void removeKey()}>Remove key</button
      >
    {/if}
  </div>

  {#if error !== null}
    <Notices notices={failureNotices} />
    <p class="error" role="alert">{error}</p>
  {/if}
  {#if probe !== null}
    <Notices notices={probe.notices} />
    <ul class="captions-results">
      {#each testLines(probe) as line}
        <li class={line.good ? "good" : "bad"}>{line.text}</li>
      {/each}
    </ul>
  {/if}
</section>
