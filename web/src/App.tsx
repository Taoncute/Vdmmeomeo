import { useState, type FormEvent } from "react";
import { api, jsonBody, setAccessToken } from "./lib/api";

interface User {
  id: string;
  email: string;
  display_name: string;
  roles: string[];
}

export default function App() {
  const [email, setEmail] = useState("admin@demo.local");
  const [password, setPassword] = useState("");
  const [user, setUser] = useState<User | null>(null);
  const [error, setError] = useState<string | null>(null);

  async function login(e: FormEvent) {
    e.preventDefault();
    setError(null);
    try {
      const res = await api<{ access_token: string; user: User }>("/auth/login", {
        method: "POST",
        ...jsonBody({ email, password }),
      });
      setAccessToken(res.access_token);
      setUser(res.user);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Lỗi không xác định");
    }
  }

  if (user) {
    return (
      <main className="p-4">
        <h1 className="text-xl font-bold">Chợ cộng đồng</h1>
        <p>
          Xin chào {user.display_name} ({user.roles.join(", ")})
        </p>
      </main>
    );
  }

  return (
    <main className="p-4 max-w-sm">
      <h1 className="text-xl font-bold mb-2">Đăng nhập</h1>
      <form onSubmit={login} className="flex flex-col gap-2">
        <input
          className="border p-1"
          value={email}
          onChange={(e) => setEmail(e.target.value)}
          placeholder="Email"
        />
        <input
          className="border p-1"
          type="password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          placeholder="Mật khẩu"
        />
        <button className="border p-1" type="submit">
          Đăng nhập
        </button>
        {error && <p className="text-red-600">{error}</p>}
      </form>
    </main>
  );
}
