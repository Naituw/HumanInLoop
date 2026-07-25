// Agent 控制台原型入口（浏览器直跑，不依赖 Tauri）。
import { createApp } from "vue";
import "../styles/tokens.css";
import "../styles/base.css";
import "../styles/controls.css";
import AgentConsoleProto from "./AgentConsoleProto.vue";

createApp(AgentConsoleProto).mount("#app");
