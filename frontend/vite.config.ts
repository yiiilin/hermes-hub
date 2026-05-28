import react from "@vitejs/plugin-react";
import { createReadStream } from "node:fs";
import { cp, mkdir, stat } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig, type Plugin } from "vite";

const projectRoot = fileURLToPath(new URL(".", import.meta.url));
const vditorDistDir = path.resolve(projectRoot, "node_modules/vditor/dist");

function contentTypeFor(filePath: string): string {
  if (filePath.endsWith(".js")) {
    return "application/javascript; charset=utf-8";
  }
  if (filePath.endsWith(".css")) {
    return "text/css; charset=utf-8";
  }
  if (filePath.endsWith(".svg")) {
    return "image/svg+xml";
  }
  if (filePath.endsWith(".png")) {
    return "image/png";
  }
  if (filePath.endsWith(".gif")) {
    return "image/gif";
  }
  return "application/octet-stream";
}

function vditorAssetsPlugin(): Plugin {
  return {
    name: "hermes-hub-vditor-assets",
    configureServer(server) {
      server.middlewares.use("/vditor/dist", (request, response, next) => {
        const rawPath = decodeURIComponent((request.url ?? "/").split("?")[0]);
        const relativePath = rawPath
          .replace(/^\/+/, "")
          .replace(/^vditor\/dist\//, "");
        const assetPath = path.resolve(vditorDistDir, relativePath);

        if (assetPath !== vditorDistDir && !assetPath.startsWith(`${vditorDistDir}${path.sep}`)) {
          response.statusCode = 403;
          response.end("Forbidden");
          return;
        }

        stat(assetPath)
          .then((assetStat) => {
            if (!assetStat.isFile()) {
              next();
              return;
            }
            response.setHeader("Content-Type", contentTypeFor(assetPath));
            createReadStream(assetPath)
              .on("error", next)
              .pipe(response);
          })
          .catch(() => next());
      });
    },
    async closeBundle() {
      const outputDir = path.resolve(projectRoot, "dist/vditor/dist");
      // Vditor 会按需加载 Lute、语言包、图标和可选渲染资源；构建产物内置这些文件，避免运行时依赖外部 CDN。
      await mkdir(path.dirname(outputDir), { recursive: true });
      await cp(vditorDistDir, outputDir, { recursive: true });
    },
  };
}

export default defineConfig({
  plugins: [react(), vditorAssetsPlugin()],
  server: {
    port: 5173,
    proxy: {
      "/api": "http://127.0.0.1:8080",
      "/internal": "http://127.0.0.1:8080",
    },
  },
});
