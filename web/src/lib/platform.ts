// Lớp trừu tượng nền tảng. Phase 1–2 chỉ có bản web (stub).
// Phase 7 (Tauri): refresh token lưu qua secure storage của hệ điều hành thay vì cookie.
export interface SecureStorage {
  get(key: string): Promise<string | null>;
  set(key: string, value: string): Promise<void>;
  remove(key: string): Promise<void>;
}

const unsupported: SecureStorage = {
  async get() {
    return null;
  },
  async set() {
    // Web dùng cookie httpOnly do server đặt, không lưu refresh token ở JS.
  },
  async remove() {},
};

export const platform = {
  isNative: false,
  secureStorage: unsupported,
};
