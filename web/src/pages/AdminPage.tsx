import { useCallback, useEffect, useState } from "react";
import { api, jsonBody } from "../lib/api";

type Tab = "deposits" | "withdrawals" | "kyc" | "refund";

interface Deposit {
  id: string;
  email: string;
  amount: number;
  gateway: string;
  transfer_code: string;
  status: string;
}

interface Withdrawal {
  id: string;
  email: string;
  amount: number;
  fee: number;
  net: number;
  status: string;
  kyc: { full_name: string; bank_name: string; account_no: string } | null;
}

interface Kyc {
  user_id: string;
  email: string;
  full_name: string;
  bank_name: string;
  account_no: string;
  status: string;
}

const vnd = (n: number) => `${n.toLocaleString("vi-VN")}đ`;

export default function AdminPage({ onLogout }: { onLogout: () => void }) {
  const [tab, setTab] = useState<Tab>("deposits");
  const [deposits, setDeposits] = useState<Deposit[]>([]);
  const [withdrawals, setWithdrawals] = useState<Withdrawal[]>([]);
  const [kyc, setKyc] = useState<Kyc[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [refundUser, setRefundUser] = useState("");
  const [refundAmount, setRefundAmount] = useState("");
  const [refundReason, setRefundReason] = useState("");

  const load = useCallback(async () => {
    setError(null);
    try {
      if (tab === "deposits") {
        const r = await api<{ deposits: Deposit[] }>("/admin/deposits?status=pending");
        setDeposits(r.deposits);
      } else if (tab === "withdrawals") {
        const r = await api<{ withdrawals: Withdrawal[] }>("/admin/withdrawals?status=pending");
        setWithdrawals(r.withdrawals);
      } else if (tab === "kyc") {
        const r = await api<{ kyc: Kyc[] }>("/admin/kyc?status=pending");
        setKyc(r.kyc);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : "Lỗi không xác định");
    }
  }, [tab]);

  useEffect(() => {
    void load();
  }, [load]);

  async function run(action: () => Promise<unknown>, done: string) {
    setError(null);
    setNotice(null);
    try {
      await action();
      setNotice(done);
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Lỗi không xác định");
    }
  }

  function ask(label: string): string | null {
    const v = window.prompt(label);
    return v && v.trim() ? v.trim() : null;
  }

  return (
    <main className="p-4">
      <header className="flex gap-2 items-center mb-2">
        <h1 className="text-xl font-bold">Quản trị</h1>
        <button className="border px-2" onClick={onLogout}>
          Đăng xuất
        </button>
      </header>
      <nav className="flex gap-2 mb-3">
        {(["deposits", "withdrawals", "kyc", "refund"] as Tab[]).map((t) => (
          <button
            key={t}
            className={`border px-2 ${t === tab ? "font-bold" : ""}`}
            onClick={() => setTab(t)}
          >
            {{ deposits: "Nạp tiền", withdrawals: "Rút tiền", kyc: "KYC", refund: "Hoàn nạp thừa" }[t]}
          </button>
        ))}
      </nav>
      {error && <p className="text-red-600">{error}</p>}
      {notice && <p className="text-green-700">{notice}</p>}

      {tab === "deposits" && (
        <table>
          <tbody>
            {deposits.map((d) => (
              <tr key={d.id}>
                <td className="pr-3">{d.email}</td>
                <td className="pr-3">{vnd(d.amount)}</td>
                <td className="pr-3">{d.transfer_code} ({d.gateway})</td>
                <td>
                  <button
                    className="border px-2"
                    onClick={() =>
                      void run(
                        () => api(`/admin/deposits/${d.id}/confirm`, { method: "POST" }),
                        "Đã xác nhận nạp tiền",
                      )
                    }
                  >
                    Xác nhận đã nhận tiền
                  </button>
                </td>
              </tr>
            ))}
            {deposits.length === 0 && <tr><td>Không có lệnh nạp đang chờ.</td></tr>}
          </tbody>
        </table>
      )}

      {tab === "withdrawals" && (
        <table>
          <tbody>
            {withdrawals.map((w) => (
              <tr key={w.id}>
                <td className="pr-3">{w.email}</td>
                <td className="pr-3">
                  Rút {vnd(w.amount)}, phí {vnd(w.fee)}, chuyển {vnd(w.net)}
                </td>
                <td className="pr-3">
                  {w.kyc ? `${w.kyc.full_name} - ${w.kyc.bank_name} - ${w.kyc.account_no}` : "Chưa có KYC"}
                </td>
                <td className="flex gap-1">
                  <button
                    className="border px-2"
                    onClick={() => {
                      const ref = ask("Mã giao dịch ngân hàng (đã chuyển khoản xong):");
                      if (ref)
                        void run(
                          () =>
                            api(`/admin/withdrawals/${w.id}/approve`, {
                              method: "POST",
                              ...jsonBody({ bank_txn_ref: ref }),
                            }),
                          "Đã duyệt lệnh rút",
                        );
                    }}
                  >
                    Duyệt
                  </button>
                  <button
                    className="border px-2"
                    onClick={() => {
                      const reason = ask("Lý do từ chối:");
                      if (reason)
                        void run(
                          () =>
                            api(`/admin/withdrawals/${w.id}/reject`, {
                              method: "POST",
                              ...jsonBody({ reason }),
                            }),
                          "Đã từ chối lệnh rút",
                        );
                    }}
                  >
                    Từ chối
                  </button>
                </td>
              </tr>
            ))}
            {withdrawals.length === 0 && <tr><td>Không có lệnh rút đang chờ.</td></tr>}
          </tbody>
        </table>
      )}

      {tab === "kyc" && (
        <table>
          <tbody>
            {kyc.map((k) => (
              <tr key={k.user_id}>
                <td className="pr-3">{k.email}</td>
                <td className="pr-3">
                  {k.full_name} - {k.bank_name} - {k.account_no}
                </td>
                <td className="flex gap-1">
                  <button
                    className="border px-2"
                    onClick={() =>
                      void run(
                        () => api(`/admin/kyc/${k.user_id}/approve`, { method: "POST" }),
                        "Đã duyệt KYC",
                      )
                    }
                  >
                    Duyệt
                  </button>
                  <button
                    className="border px-2"
                    onClick={() => {
                      const reason = ask("Lý do từ chối:");
                      if (reason)
                        void run(
                          () =>
                            api(`/admin/kyc/${k.user_id}/reject`, {
                              method: "POST",
                              ...jsonBody({ reason }),
                            }),
                          "Đã từ chối KYC",
                        );
                    }}
                  >
                    Từ chối
                  </button>
                </td>
              </tr>
            ))}
            {kyc.length === 0 && <tr><td>Không có hồ sơ KYC đang chờ.</td></tr>}
          </tbody>
        </table>
      )}

      {tab === "refund" && (
        <form
          className="flex flex-col gap-2 max-w-sm"
          onSubmit={(e) => {
            e.preventDefault();
            void run(
              () =>
                api(`/admin/users/${refundUser.trim()}/refund-deposit`, {
                  method: "POST",
                  headers: { "Idempotency-Key": crypto.randomUUID() },
                  ...jsonBody({ amount: Number(refundAmount), reason: refundReason }),
                }),
              "Đã hoàn tiền nạp thừa",
            );
          }}
        >
          <input className="border p-1" placeholder="ID người dùng" value={refundUser} onChange={(e) => setRefundUser(e.target.value)} />
          <input className="border p-1" placeholder="Số tiền (đồng)" value={refundAmount} onChange={(e) => setRefundAmount(e.target.value)} />
          <input className="border p-1" placeholder="Lý do (bắt buộc)" value={refundReason} onChange={(e) => setRefundReason(e.target.value)} />
          <button className="border p-1" type="submit">Hoàn tiền</button>
        </form>
      )}
    </main>
  );
}
