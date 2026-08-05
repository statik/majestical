<!-- The empty `<script lang="ts">` is load-bearing: svelte2tsx only emits a
     component type for a .svelte file that has a script block, and without one
     `import App from "./App.svelte"` is an untyped import svelte-check
     rejects. Task 8 fills this in. -->
<script lang="ts"></script>

<main>
  <h1>Majestical</h1>
</main>
