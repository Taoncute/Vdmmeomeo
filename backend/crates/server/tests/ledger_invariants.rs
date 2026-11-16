mod common;

use std::collections::BTreeMap;

use common::*;
use ledger::{kinds, AccountKey, LedgerError, PostOutcome, TxBuilder};
use proptest::prelude::*;
use proptest::strategy::{Strategy, ValueTree};
use proptest::test_runner::TestRunner;
use server::domain::stall::Source;
use server::domain::{deposits, escrow, refund, stall, withdrawals};
use sqlx::PgPool;
use tokio::task::JoinSet;
use uuid::Uuid;

/// Ghi tay các bút toán vào một giao dịch DB, bỏ qua mã Rust, để thử ràng buộc của chính Postgres.
async fn raw_tx(pool: &PgPool, entries: &[(&str, i64)]) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    let tx_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO ledger_transactions (kind, idempotency_key) VALUES ('raw', $1) RETURNING id",
    )
    .bind(Uuid::new_v4().to_string())
    .fetch_one(&mut *tx)
    .await?;
    for (key, amount) in entries {
        let acc = sqlx::query_scalar::<_, Uuid>("SELECT id FROM ledger_accounts WHERE key = $1")
            .bind(*key)
            .fetch_one(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO ledger_entries (tx_id, account_id, amount) VALUES ($1, $2, $3)")
            .bind(tx_id)
            .bind(acc)
            .bind(*amount)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn db_tu_choi_giao_dich_tong_khac_0(pool: PgPool) {
    for bad in [
        vec![("platform_revenue", 100)],
        vec![("platform_revenue", 100), ("external", -50)],
        vec![
            ("platform_revenue", 100),
            ("external", -100),
            ("external", 1),
        ],
    ] {
        let err = raw_tx(&pool, &bad)
            .await
            .expect_err("giao dịch lệch phải bị DB từ chối");
        assert!(err.to_string().contains("not balanced"), "{err}");
    }
    // Tổng bằng 0 thì qua.
    raw_tx(&pool, &[("platform_revenue", 100), ("external", -100)])
        .await
        .expect("giao dịch cân phải được chấp nhận");
    assert_eq!(count(&pool, "SELECT count(*) FROM ledger_entries").await, 2);
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn db_chan_sua_xoa_but_toan(pool: PgPool) {
    let u = create_user(&pool, "a@example.com").await;
    fund(&pool, AccountKey::wallet_spendable(u), 1000).await;
    for sql in [
        "UPDATE ledger_entries SET amount = 1",
        "DELETE FROM ledger_entries",
        "UPDATE ledger_transactions SET kind = 'x'",
        "DELETE FROM ledger_transactions",
    ] {
        let err = sqlx::query(sql)
            .execute(&pool)
            .await
            .expect_err(&format!("{sql} phải bị chặn"));
        assert!(err.to_string().contains("append-only"), "{sql}: {err}");
    }
    // TRUNCATE bị chặn (bằng trigger, hoặc bằng khóa ngoại với bảng được tham chiếu).
    for sql in ["TRUNCATE ledger_entries", "TRUNCATE ledger_transactions"] {
        assert!(
            sqlx::query(sql).execute(&pool).await.is_err(),
            "{sql} phải bị chặn"
        );
    }
    assert_eq!(count(&pool, "SELECT count(*) FROM ledger_entries").await, 2);
    assert_books_clean(&pool).await;
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn db_chan_so_du_am_tru_external(pool: PgPool) {
    let u = create_user(&pool, "a@example.com").await;
    fund(&pool, AccountKey::wallet_spendable(u), 1000).await;
    let wallet = format!("wallet_spendable:{u}");
    let err = sqlx::query(
        "UPDATE account_balances SET balance = -1 \
         WHERE account_id = (SELECT id FROM ledger_accounts WHERE key = $1)",
    )
    .bind(wallet)
    .execute(&pool)
    .await
    .expect_err("số dư âm phải bị CHECK chặn");
    assert!(server::error::is_check_violation(&err), "{err}");
    // external là phía ngoài hệ thống nên được âm (sau khi nạp nó đang âm).
    assert_eq!(balance(&pool, AccountKey::external()).await, -1000);
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn ledger_post_idempotent_va_khong_am(pool: PgPool) {
    let u = create_user(&pool, "a@example.com").await;
    let wallet = AccountKey::wallet_spendable(u);
    fund(&pool, wallet, 100).await;

    // Trừ quá số dư: bị từ chối, số dư giữ nguyên.
    let over = TxBuilder::new(kinds::STALL_RENT, "k-over")
        .debit(wallet, 150)
        .credit(AccountKey::platform_revenue(), 150)
        .build()
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let r = ledger::post(&mut tx, &over).await;
    assert!(matches!(
        r,
        Err(LedgerError::InsufficientFunds {
            balance: 100,
            needed: 150,
            ..
        })
    ));
    drop(tx);
    assert_eq!(balance(&pool, wallet).await, 100);

    // Cùng idempotency_key: lần hai không ghi thêm.
    let ok = TxBuilder::new(kinds::STALL_RENT, "k-ok")
        .debit(wallet, 40)
        .credit(AccountKey::platform_revenue(), 40)
        .build()
        .unwrap();
    let mut outcomes = Vec::new();
    for _ in 0..3 {
        let mut tx = pool.begin().await.unwrap();
        outcomes.push(ledger::post(&mut tx, &ok).await.unwrap());
        tx.commit().await.unwrap();
    }
    assert!(outcomes[0].is_new());
    assert!(!outcomes[1].is_new() && !outcomes[2].is_new());
    assert_eq!(outcomes[1].tx_id(), outcomes[0].tx_id());
    assert_eq!(balance(&pool, wallet).await, 60);
    assert_eq!(balance(&pool, AccountKey::platform_revenue()).await, 40);
    assert_books_clean(&pool).await;
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn ghi_dong_thoi_cung_idempotency_key_chi_mot_lan(pool: PgPool) {
    let u = create_user(&pool, "a@example.com").await;
    let wallet = AccountKey::wallet_spendable(u);
    fund(&pool, wallet, 1000).await;
    let big = big_pool(&pool).await;

    let mut set = JoinSet::new();
    for _ in 0..12 {
        let db = big.clone();
        set.spawn(async move {
            let t = TxBuilder::new(kinds::STALL_RENT, "k-concurrent")
                .debit(wallet, 300)
                .credit(AccountKey::platform_revenue(), 300)
                .build()
                .unwrap();
            let mut tx = db.begin().await.unwrap();
            let out = ledger::post(&mut tx, &t).await.unwrap();
            tx.commit().await.unwrap();
            out
        });
    }
    let mut created = 0;
    while let Some(res) = set.join_next().await {
        if matches!(res.unwrap(), PostOutcome::Created { .. }) {
            created += 1;
        }
    }
    assert_eq!(created, 1, "chỉ một lần được ghi");
    assert_eq!(balance(&pool, wallet).await, 700);
    assert_books_clean(&pool).await;
}

// ---------- Property test: chuỗi thao tác ngẫu nhiên trên Postgres thật ----------

#[derive(Debug, Clone)]
enum Op {
    Deposit { user: usize, amount: i64 },
    Hold { order: usize, amount: i64 },
    Complete { order: usize },
    ReleaseDue,
    RefundEscrow { order: usize },
    RefundPending { order: usize },
    CreateWithdrawal { user: usize, amount: i64 },
    ApproveWithdrawal { pick: usize },
    RejectWithdrawal { pick: usize },
    Rent { user: usize, earnings: bool },
    RefundDeposit { user: usize, amount: i64 },
}

fn op_strategy() -> impl Strategy<Value = Op> {
    // prop_oneof! chỉ nhận tối đa 10 nhánh nên chia thành ba nhóm lồng nhau.
    let money = prop_oneof![
        3 => (0..4usize, 1..200_000i64).prop_map(|(user, amount)| Op::Deposit { user, amount }),
        1 => (0..4usize, 1..50_000i64).prop_map(|(user, amount)| Op::RefundDeposit { user, amount }),
        2 => (0..4usize, any::<bool>()).prop_map(|(user, earnings)| Op::Rent { user, earnings }),
    ];
    let escrow_ops = prop_oneof![
        3 => (0..4usize, 1..100_000i64).prop_map(|(order, amount)| Op::Hold { order, amount }),
        2 => (0..4usize).prop_map(|order| Op::Complete { order }),
        2 => Just(Op::ReleaseDue),
        1 => (0..4usize).prop_map(|order| Op::RefundEscrow { order }),
        1 => (0..4usize).prop_map(|order| Op::RefundPending { order }),
    ];
    let withdrawal_ops = prop_oneof![
        3 => (0..4usize, 1..150_000i64).prop_map(|(user, amount)| Op::CreateWithdrawal { user, amount }),
        2 => (0..8usize).prop_map(|pick| Op::ApproveWithdrawal { pick }),
        2 => (0..8usize).prop_map(|pick| Op::RejectWithdrawal { pick }),
    ];
    prop_oneof![4 => money, 5 => escrow_ops, 4 => withdrawal_ops]
}

/// Lỗi nghiệp vụ được phép (từ chối hợp lệ). Lỗi nội bộ (kể cả vi phạm CHECK của DB) là bug.
fn expect_ok_or_business<T>(r: server::error::AppResult<T>, ctx: &str, ops: &[Op]) {
    if let Err(e) = r {
        assert!(
            !matches!(e, server::error::AppError::Internal(_)),
            "{ctx} gây lỗi nội bộ: {e}\nchuỗi thao tác: {ops:#?}"
        );
    }
}

async fn run_sequence(pool: &PgPool, ops: &[Op]) {
    let run = Uuid::new_v4().simple().to_string();
    let admin = create_admin(pool, &format!("admin-{run}@example.com")).await;
    let mut users = Vec::new();
    for i in 0..4 {
        let id = create_user(pool, &format!("u{i}-{run}@example.com")).await;
        set_kyc(pool, id, "approved").await;
        users.push(id);
    }
    // Đơn hàng i: người mua i, người bán (i + 1) % 4.
    let orders: Vec<Uuid> = (0..4).map(|_| Uuid::new_v4()).collect();
    let mut held: BTreeMap<usize, i64> = BTreeMap::new();
    let mut shop_seq = 0;

    for op in ops {
        match op {
            Op::Deposit { user, amount } => {
                let d = deposits::create(pool, users[*user], *amount, "fake")
                    .await
                    .unwrap();
                let r =
                    deposits::confirm(pool, d.id, Some(*amount), deposits::ConfirmSource::Webhook)
                        .await;
                expect_ok_or_business(r, "deposit", ops);
            }
            Op::Hold { order, amount } => {
                let r = escrow::hold(pool, users[*order], orders[*order], *amount).await;
                if matches!(&r, Ok(o) if o.is_new()) {
                    held.insert(*order, *amount);
                }
                expect_ok_or_business(r, "hold", ops);
            }
            Op::Complete { order } => {
                let amount = held.get(order).copied().unwrap_or(1);
                let r =
                    escrow::complete(pool, users[(*order + 1) % 4], orders[*order], amount).await;
                expect_ok_or_business(r, "complete", ops);
            }
            Op::ReleaseDue => {
                sqlx::query("UPDATE earnings_holds SET release_at = now() - interval '1 second' WHERE status = 'pending'")
                    .execute(pool)
                    .await
                    .unwrap();
                expect_ok_or_business(escrow::release_due(pool, 100).await, "release_due", ops);
            }
            Op::RefundEscrow { order } => {
                let amount = held.get(order).copied().unwrap_or(1);
                let r =
                    escrow::refund_from_escrow(pool, users[*order], orders[*order], amount).await;
                expect_ok_or_business(r, "refund_from_escrow", ops);
            }
            Op::RefundPending { order } => {
                let amount = held.get(order).copied().unwrap_or(1);
                let r = escrow::refund_from_pending(
                    pool,
                    users[(*order + 1) % 4],
                    users[*order],
                    orders[*order],
                    amount,
                )
                .await;
                expect_ok_or_business(r, "refund_from_pending", ops);
            }
            Op::CreateWithdrawal { user, amount } => {
                let r = withdrawals::create(pool, users[*user], *amount, None).await;
                expect_ok_or_business(r, "withdrawal_create", ops);
            }
            Op::ApproveWithdrawal { pick } | Op::RejectWithdrawal { pick } => {
                let pending = sqlx::query_scalar::<_, Uuid>(
                    "SELECT id FROM withdrawals WHERE status = 'pending' ORDER BY created_at, id",
                )
                .fetch_all(pool)
                .await
                .unwrap();
                if pending.is_empty() {
                    continue;
                }
                let id = pending[pick % pending.len()];
                let ctx = admin_ctx(admin);
                if matches!(op, Op::ApproveWithdrawal { .. }) {
                    expect_ok_or_business(
                        withdrawals::approve(pool, ctx, id, "MGD-1").await,
                        "approve",
                        ops,
                    );
                } else {
                    expect_ok_or_business(
                        withdrawals::reject(pool, ctx, id, "không hợp lệ").await,
                        "reject",
                        ops,
                    );
                }
            }
            Op::Rent { user, earnings } => {
                shop_seq += 1;
                let source = if *earnings {
                    Source::Earnings
                } else {
                    Source::Spendable
                };
                let name = format!("S{} {shop_seq}", &run[..8]);
                let r = stall::rent(pool, users[*user], source, Some(name.as_str()), None).await;
                expect_ok_or_business(r, "rent", ops);
            }
            Op::RefundDeposit { user, amount } => {
                let r = refund::refund_deposit(
                    pool,
                    admin_ctx(admin),
                    users[*user],
                    *amount,
                    "nạp thừa",
                    None,
                )
                .await;
                expect_ok_or_business(r, "refund_deposit", ops);
            }
        }
    }
}

/// Sau mọi chuỗi thao tác: tổng sổ bằng 0, mọi giao dịch cân, `balance` bằng tổng bút toán,
/// không ví nào âm, tiền người dùng + doanh thu sàn + external cộng lại bằng 0.
#[sqlx::test(migrator = "server::MIGRATOR")]
async fn property_so_cai_luon_can_bang(pool: PgPool) {
    // Hạ ngưỡng để các nhánh rút tiền và thuê gian hàng thực sự chạy được.
    set_setting(&pool, "withdraw_min", serde_json::json!(1)).await;
    set_setting(&pool, "stall_price", serde_json::json!(1000)).await;

    let strategy = proptest::collection::vec(op_strategy(), 5..40);
    let mut runner = TestRunner::default();
    for case in 0..20 {
        let ops = strategy
            .new_tree(&mut runner)
            .expect("sinh chuỗi thao tác")
            .current();
        run_sequence(&pool, &ops).await;
        let mut conn = pool.acquire().await.unwrap();
        let report = ledger::store::verify_books(&mut conn).await.unwrap();
        assert!(
            report.is_clean(),
            "ca {case}: sổ cái lệch: {report:?}\nchuỗi thao tác: {ops:#?}"
        );
        let sum_all: i64 =
            sqlx::query_scalar("SELECT COALESCE(SUM(balance), 0)::bigint FROM account_balances")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(sum_all, 0, "ca {case}: tổng mọi số dư phải bằng 0");
    }
}
