declare interface Window {
  electronAPI?: any;
  __PS5UPLOAD_WEB_MODE__?: boolean;
  ps5uploadWebAPI?: {
    invoke?: (cmd: string, args?: Record<string, unknown>) => Promise<any>;
    on?: (event: string, cb: (payload: any) => void) => () => void;
    once?: (event: string, cb: (payload: any) => void) => void;
  };
}
