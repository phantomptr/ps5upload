// Electron adapter for window API

export class LogicalPosition {
  x: number;
  y: number;

  constructor(x: number, y: number) {
    this.x = x;
    this.y = y;
  }
}

export class LogicalSize {
  width: number;
  height: number;

  constructor(width: number, height: number) {
    this.width = width;
    this.height = height;
  }
}

export interface Monitor {
  name: string | null;
  size: { width: number; height: number };
  position: { x: number; y: number };
  scaleFactor: number;
  workArea?: {
    position: { x: number; y: number };
    size: { width: number; height: number };
  };
}

export const appWindow = {
  minimize: () => window.electronAPI?.windowMinimize(),
  maximize: () => window.electronAPI?.windowMaximize(),
  toggleMaximize: () => window.electronAPI?.windowMaximize(),
  close: () => window.electronAPI?.windowClose(),
  onResized: (callback: (event: { payload: { width: number; height: number } }) => void) => {
    const handler = () => {
      callback({
        payload: { width: window.innerWidth, height: window.innerHeight }
      });
    };
    window.addEventListener('resize', handler);
    return () => {
      window.removeEventListener('resize', handler);
    };
  },
  onMoved: (callback: (event: { payload: { x: number; y: number } }) => void) => {
    let lastX = window.screenX;
    let lastY = window.screenY;
    const interval = setInterval(() => {
      if (window.screenX !== lastX || window.screenY !== lastY) {
        lastX = window.screenX;
        lastY = window.screenY;
        callback({ payload: { x: lastX, y: lastY } });
      }
    }, 500);
    return () => {
      clearInterval(interval);
    };
  },
  innerSize: async () => ({ width: window.innerWidth, height: window.innerHeight }),
  outerPosition: async () => ({ x: window.screenX, y: window.screenY }),
  isMaximized: async () => window.innerWidth >= screen.availWidth && window.innerHeight >= screen.availHeight,
  setSize: async () => {},
  setPosition: async () => {},
};

export function getCurrentWindow() {
  return appWindow;
}

export function currentMonitor(): Promise<Monitor | null> {
  if (window.electronAPI?.currentMonitor) {
    return window.electronAPI.currentMonitor();
  }
  return Promise.resolve({
    name: 'Primary',
    size: { width: screen.width, height: screen.height },
    position: { x: 0, y: 0 },
    scaleFactor: window.devicePixelRatio || 1,
    workArea: {
      position: { x: 0, y: 0 },
      size: { width: screen.availWidth, height: screen.availHeight },
    },
  });
}
