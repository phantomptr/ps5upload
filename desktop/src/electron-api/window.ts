// Electron adapter for Tauri window API
// This provides compatibility with Tauri's window module

import './types';

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

type ResizeCallback = (event: { payload: { width: number; height: number } }) => void;
type MoveCallback = (event: { payload: { x: number; y: number } }) => void;

class AppWindow {
  private resizeListeners: ResizeCallback[] = [];
  private moveListeners: MoveCallback[] = [];

  async minimize(): Promise<void> {
    if (window.electronAPI?.windowMinimize) {
      return window.electronAPI.windowMinimize();
    }
    console.warn('Window minimize not available');
  }

  async maximize(): Promise<void> {
    if (window.electronAPI?.windowMaximize) {
      return window.electronAPI.windowMaximize();
    }
    console.warn('Window maximize not available');
  }

  async toggleMaximize(): Promise<void> {
    // Same as maximize in our implementation
    if (window.electronAPI?.windowMaximize) {
      return window.electronAPI.windowMaximize();
    }
    console.warn('Window toggleMaximize not available');
  }

  async close(): Promise<void> {
    if (window.electronAPI?.windowClose) {
      return window.electronAPI.windowClose();
    }
    console.warn('Window close not available');
  }

  async setSize(_size: LogicalSize): Promise<void> {
    // Not implemented in Electron adapter - window size is managed by Electron main process
    console.warn('Window setSize not implemented');
  }

  async setPosition(_position: LogicalPosition): Promise<void> {
    // Not implemented in Electron adapter
    console.warn('Window setPosition not implemented');
  }

  async innerSize(): Promise<LogicalSize> {
    return new LogicalSize(window.innerWidth, window.innerHeight);
  }

  async outerPosition(): Promise<LogicalPosition> {
    return new LogicalPosition(window.screenX, window.screenY);
  }

  async isMaximized(): Promise<boolean> {
    // Approximation
    return window.innerWidth >= screen.availWidth && window.innerHeight >= screen.availHeight;
  }

  async onResized(callback: ResizeCallback): Promise<() => void> {
    const handler = () => {
      callback({
        payload: { width: window.innerWidth, height: window.innerHeight }
      });
    };
    window.addEventListener('resize', handler);
    this.resizeListeners.push(callback);
    return () => {
      window.removeEventListener('resize', handler);
      const idx = this.resizeListeners.indexOf(callback);
      if (idx !== -1) this.resizeListeners.splice(idx, 1);
    };
  }

  async onMoved(callback: MoveCallback): Promise<() => void> {
    // In browser context, we can't easily track window moves
    // This is a simplified implementation
    let lastX = window.screenX;
    let lastY = window.screenY;

    const interval = setInterval(() => {
      if (window.screenX !== lastX || window.screenY !== lastY) {
        lastX = window.screenX;
        lastY = window.screenY;
        callback({ payload: { x: lastX, y: lastY } });
      }
    }, 500);

    this.moveListeners.push(callback);
    return () => {
      clearInterval(interval);
      const idx = this.moveListeners.indexOf(callback);
      if (idx !== -1) this.moveListeners.splice(idx, 1);
    };
  }
}

const appWindow = new AppWindow();

export function getCurrentWindow(): AppWindow {
  return appWindow;
}

export async function currentMonitor(): Promise<Monitor | null> {
  return {
    name: 'Primary',
    size: { width: screen.width, height: screen.height },
    position: { x: 0, y: 0 },
    scaleFactor: window.devicePixelRatio || 1,
    workArea: {
      position: { x: 0, y: 0 },
      size: { width: screen.availWidth, height: screen.availHeight },
    },
  };
}
