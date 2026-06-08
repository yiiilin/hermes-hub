import react from "@vitejs/plugin-react";
import { createReadStream } from "node:fs";
import { cp, mkdir, readFile, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig, type Plugin } from "vite";

const projectRoot = fileURLToPath(new URL(".", import.meta.url));
const vditorDistDir = path.resolve(projectRoot, "node_modules/vditor/dist");
const includeHermesHubExample = process.env.HERMES_HUB_BUILD_HERMES_EXAMPLE === "1";

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
  let outputDir = path.resolve(projectRoot, "dist");

  return {
    name: "hermes-hub-vditor-assets",
    configResolved(config) {
      outputDir = path.resolve(projectRoot, config.build.outDir);
    },
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
      const vditorOutputDir = path.resolve(outputDir, "vditor/dist");
      // Vditor 会按需加载 Lute、语言包、图标和可选渲染资源；构建产物内置这些文件，避免运行时依赖外部 CDN。
      await mkdir(path.dirname(vditorOutputDir), { recursive: true });
      await cp(vditorDistDir, vditorOutputDir, { recursive: true });
    },
  };
}

function exampleServiceWorkerManifestPlugin(): Plugin {
  let outputDir = path.resolve(projectRoot, "dist");

  return {
    name: "hermes-hub-example-service-worker-manifest",
    configResolved(config) {
      outputDir = path.resolve(projectRoot, config.build.outDir);
    },
    async closeBundle() {
      const exampleHtmlPath = path.resolve(outputDir, "examples/hermes-hub/index.html");
      const serviceWorkerPath = path.resolve(outputDir, "service-worker.js");

      const exampleHtml = await readFile(exampleHtmlPath, "utf8");
      const assetMatches = Array.from(
        exampleHtml.matchAll(/(?:src|href)="(\/assets\/[^"]+\.(?:js|css))"/g),
      ).map((match) => match[1]!);
      const uniqueAssets = Array.from(new Set(assetMatches));

      if (uniqueAssets.length === 0) {
        return;
      }

      const serviceWorkerSource = await readFile(serviceWorkerPath, "utf8");
      const manifestPrelude = `self.EXAMPLE_ASSET_MANIFEST = ${JSON.stringify(uniqueAssets)};\n`;

      if (serviceWorkerSource.startsWith("self.EXAMPLE_ASSET_MANIFEST = ")) {
        const replacedSource = serviceWorkerSource.replace(
          /^self\.EXAMPLE_ASSET_MANIFEST = .*?;\n/,
          manifestPrelude,
        );
        await writeFile(serviceWorkerPath, replacedSource, "utf8");
        return;
      }

      await writeFile(serviceWorkerPath, `${manifestPrelude}${serviceWorkerSource}`, "utf8");
    },
  };
}

export default defineConfig({
  plugins: [
    react(),
    vditorAssetsPlugin(),
    // 正式构建默认不再产出 /examples；只有显式构建 demo 时才把 example 资源清单注入 service worker。
    ...(includeHermesHubExample ? [exampleServiceWorkerManifestPlugin()] : []),
  ],
  build: {
    rollupOptions: {
      input: {
        main: path.resolve(projectRoot, "index.html"),
        ...(includeHermesHubExample
          ? {
              hermesHubExample: path.resolve(projectRoot, "examples/hermes-hub/index.html"),
            }
          : {}),
      },
    },
  },
  server: {
    port: 5173,
    proxy: {
      "/api": "http://127.0.0.1:8080",
      "/internal": "http://127.0.0.1:8080",
    },
  },
});
