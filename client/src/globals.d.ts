/// <reference types="vite/client" />
// NB: the triple-slash reference MUST be the first statement in the file
// — TypeScript 6 ignores directives that follow other declarations, which
// dropped vite/client's `*.css` (and friends) ambient modules and broke
// the side-effect `import "./index.css"`.

declare interface Window {
  electronAPI?: any;
  __PS5UPLOAD_WEB_MODE__?: boolean;
  ps5uploadWebAPI?: {
    invoke?: (cmd: string, args?: Record<string, unknown>) => Promise<any>;
    on?: (event: string, cb: (payload: any) => void) => () => void;
    once?: (event: string, cb: (payload: any) => void) => void;
  };
}
interface ImportMetaEnv {
  readonly DEV: boolean;
  readonly PROD: boolean;
  readonly MODE: string;
}
interface ImportMeta {
  readonly env: ImportMetaEnv;
}
