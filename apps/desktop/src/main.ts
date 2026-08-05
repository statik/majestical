import { mount } from "svelte";
import App from "./App.svelte";
import "./app.css";

const target = document.querySelector("#app");
if (!target) {
  throw new Error("mount point #app is missing from index.html");
}

mount(App, { target });
