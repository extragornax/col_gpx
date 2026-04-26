use rusqlite::{params, Connection};
use std::sync::Mutex;

pub struct Db {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ClimbRecord {
    pub id: i64,
    pub name: Option<String>,
    pub lat: f64,
    pub lon: f64,
    pub start_ele: f64,
    pub end_ele: f64,
    pub gain: f64,
    pub distance_km: f64,
    pub gradient: f64,
    pub times_ridden: i64,
    pub first_ridden: String,
    pub last_ridden: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ClimbAttempt {
    pub id: i64,
    pub climb_id: i64,
    pub activity_date: String,
    pub activity_name: Option<String>,
    pub elapsed_seconds: Option<f64>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Stats {
    pub total_climbs: i64,
    pub total_attempts: i64,
    pub total_gain_m: f64,
    pub highest_climb_m: f64,
    pub steepest_gradient: f64,
    pub most_ridden_climb: Option<String>,
    pub most_ridden_count: i64,
}

pub struct StravaTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    pub athlete_id: i64,
    pub athlete_name: Option<String>,
}

impl Db {
    pub fn open(path: &str) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000; PRAGMA foreign_keys=ON;")?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn migrate(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS users (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                username      TEXT NOT NULL UNIQUE COLLATE NOCASE,
                password_hash TEXT NOT NULL,
                share_id      TEXT NOT NULL UNIQUE DEFAULT (hex(randomblob(8))),
                created_at    TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS sessions (
                token      TEXT PRIMARY KEY,
                user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                expires_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS climbs (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                user_id     INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                name        TEXT,
                lat         REAL NOT NULL,
                lon         REAL NOT NULL,
                start_ele   REAL NOT NULL,
                end_ele     REAL NOT NULL,
                gain        REAL NOT NULL,
                distance_km REAL NOT NULL,
                gradient    REAL NOT NULL,
                first_ridden TEXT NOT NULL,
                last_ridden  TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS attempts (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                climb_id        INTEGER NOT NULL REFERENCES climbs(id) ON DELETE CASCADE,
                activity_date   TEXT NOT NULL,
                activity_name   TEXT,
                elapsed_seconds REAL
            );

            CREATE TABLE IF NOT EXISTS strava_tokens (
                user_id       INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
                access_token  TEXT NOT NULL,
                refresh_token TEXT NOT NULL,
                expires_at    INTEGER NOT NULL,
                athlete_id    INTEGER NOT NULL,
                athlete_name  TEXT
            );

            CREATE TABLE IF NOT EXISTS synced_activities (
                strava_id   INTEGER NOT NULL,
                user_id     INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                PRIMARY KEY (strava_id, user_id)
            );

            CREATE INDEX IF NOT EXISTS idx_climbs_user ON climbs(user_id);
            CREATE INDEX IF NOT EXISTS idx_climbs_loc ON climbs(user_id, lat, lon);
            CREATE INDEX IF NOT EXISTS idx_attempts_climb ON attempts(climb_id);
            CREATE INDEX IF NOT EXISTS idx_sessions_token ON sessions(token);
            CREATE INDEX IF NOT EXISTS idx_strava_athlete ON strava_tokens(athlete_id);"
        )?;

        // Migration: add share_id to existing users table if missing
        let has_share_id: bool = conn
            .prepare("SELECT share_id FROM users LIMIT 0")
            .is_ok();
        if !has_share_id {
            conn.execute_batch(
                "ALTER TABLE users ADD COLUMN share_id TEXT UNIQUE DEFAULT (hex(randomblob(8)));
                 UPDATE users SET share_id = hex(randomblob(8)) WHERE share_id IS NULL;"
            )?;
        }

        Ok(())
    }

    // ── Users ──

    pub fn create_user(&self, username: &str, password_hash: &str, share_id: &str) -> anyhow::Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO users (username, password_hash, share_id) VALUES (?1, ?2, ?3)",
            params![username, password_hash, share_id],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_user_by_id(&self, id: i64) -> anyhow::Result<Option<(i64, String)>> {
        let conn = self.conn.lock().unwrap();
        let result = conn.query_row(
            "SELECT id, username FROM users WHERE id = ?1",
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        );
        match result {
            Ok(u) => Ok(Some(u)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn get_user_by_share_id(&self, share_id: &str) -> anyhow::Result<Option<i64>> {
        let conn = self.conn.lock().unwrap();
        let result = conn.query_row(
            "SELECT id FROM users WHERE share_id = ?1",
            params![share_id],
            |row| row.get(0),
        );
        match result {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn get_share_id(&self, user_id: i64) -> anyhow::Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let result = conn.query_row(
            "SELECT share_id FROM users WHERE id = ?1",
            params![user_id],
            |row| row.get(0),
        );
        match result {
            Ok(s) => Ok(Some(s)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn regenerate_share_id(&self, user_id: i64, new_share_id: &str) -> anyhow::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "UPDATE users SET share_id = ?2 WHERE id = ?1",
            params![user_id, new_share_id],
        )?;
        Ok(n > 0)
    }

    pub fn get_user_by_username(&self, username: &str) -> anyhow::Result<Option<(i64, String)>> {
        let conn = self.conn.lock().unwrap();
        let result = conn.query_row(
            "SELECT id, password_hash FROM users WHERE username = ?1",
            params![username],
            |row| Ok((row.get(0)?, row.get(1)?)),
        );
        match result {
            Ok(u) => Ok(Some(u)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    // ── Sessions ──

    pub fn create_session(&self, token: &str, user_id: i64) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO sessions (token, user_id, expires_at) VALUES (?1, ?2, datetime('now', '+30 days'))",
            params![token, user_id],
        )?;
        Ok(())
    }

    pub fn get_session(&self, token: &str) -> anyhow::Result<Option<i64>> {
        let conn = self.conn.lock().unwrap();
        let result = conn.query_row(
            "SELECT user_id FROM sessions WHERE token = ?1 AND expires_at > datetime('now')",
            params![token],
            |row| row.get(0),
        );
        match result {
            Ok(uid) => Ok(Some(uid)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn delete_session(&self, token: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM sessions WHERE token = ?1", params![token])?;
        Ok(())
    }

    // ── Climbs ──

    pub fn find_nearby_climb(&self, user_id: i64, lat: f64, lon: f64, radius_km: f64) -> anyhow::Result<Option<i64>> {
        let conn = self.conn.lock().unwrap();
        let dlat = radius_km / 111.0;
        let dlon = radius_km / (111.0 * lat.to_radians().cos());

        let mut stmt = conn.prepare(
            "SELECT id, lat, lon FROM climbs
             WHERE user_id = ?1 AND lat BETWEEN ?2 AND ?3 AND lon BETWEEN ?4 AND ?5"
        )?;
        let rows: Vec<(i64, f64, f64)> = stmt.query_map(
            params![user_id, lat - dlat, lat + dlat, lon - dlon, lon + dlon],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?.collect::<Result<Vec<_>, _>>()?;

        for (id, clat, clon) in rows {
            if haversine_km(lat, lon, clat, clon) < radius_km {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert_climb(
        &self, user_id: i64, lat: f64, lon: f64,
        start_ele: f64, end_ele: f64, gain: f64,
        distance_km: f64, gradient: f64, date: &str,
    ) -> anyhow::Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO climbs (user_id, lat, lon, start_ele, end_ele, gain, distance_km, gradient, first_ridden, last_ridden)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
            params![user_id, lat, lon, start_ele, end_ele, gain, distance_km, gradient, date],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn add_attempt(&self, climb_id: i64, date: &str, name: Option<&str>, elapsed: Option<f64>) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO attempts (climb_id, activity_date, activity_name, elapsed_seconds) VALUES (?1, ?2, ?3, ?4)",
            params![climb_id, date, name, elapsed],
        )?;
        conn.execute(
            "UPDATE climbs SET last_ridden = MAX(last_ridden, ?2) WHERE id = ?1",
            params![climb_id, date],
        )?;
        Ok(())
    }

    pub fn get_climbs(&self, user_id: i64) -> anyhow::Result<Vec<ClimbRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT c.id, c.name, c.lat, c.lon, c.start_ele, c.end_ele, c.gain, c.distance_km,
                    c.gradient, COUNT(a.id), c.first_ridden, c.last_ridden
             FROM climbs c LEFT JOIN attempts a ON a.climb_id = c.id
             WHERE c.user_id = ?1 GROUP BY c.id ORDER BY c.gain DESC"
        )?;
        let rows = stmt.query_map(params![user_id], |row| {
            Ok(ClimbRecord {
                id: row.get(0)?, name: row.get(1)?, lat: row.get(2)?, lon: row.get(3)?,
                start_ele: row.get(4)?, end_ele: row.get(5)?, gain: row.get(6)?,
                distance_km: row.get(7)?, gradient: row.get(8)?, times_ridden: row.get(9)?,
                first_ridden: row.get(10)?, last_ridden: row.get(11)?,
            })
        })?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_climb(&self, user_id: i64, id: i64) -> anyhow::Result<Option<ClimbRecord>> {
        let conn = self.conn.lock().unwrap();
        let result = conn.query_row(
            "SELECT c.id, c.name, c.lat, c.lon, c.start_ele, c.end_ele, c.gain, c.distance_km,
                    c.gradient, COUNT(a.id), c.first_ridden, c.last_ridden
             FROM climbs c LEFT JOIN attempts a ON a.climb_id = c.id
             WHERE c.id = ?1 AND c.user_id = ?2 GROUP BY c.id",
            params![id, user_id],
            |row| Ok(ClimbRecord {
                id: row.get(0)?, name: row.get(1)?, lat: row.get(2)?, lon: row.get(3)?,
                start_ele: row.get(4)?, end_ele: row.get(5)?, gain: row.get(6)?,
                distance_km: row.get(7)?, gradient: row.get(8)?, times_ridden: row.get(9)?,
                first_ridden: row.get(10)?, last_ridden: row.get(11)?,
            }),
        );
        match result {
            Ok(c) => Ok(Some(c)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn get_attempts(&self, climb_id: i64) -> anyhow::Result<Vec<ClimbAttempt>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, climb_id, activity_date, activity_name, elapsed_seconds
             FROM attempts WHERE climb_id = ?1 ORDER BY activity_date DESC"
        )?;
        let rows = stmt.query_map(params![climb_id], |row| {
            Ok(ClimbAttempt {
                id: row.get(0)?, climb_id: row.get(1)?, activity_date: row.get(2)?,
                activity_name: row.get(3)?, elapsed_seconds: row.get(4)?,
            })
        })?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn rename_climb(&self, user_id: i64, id: i64, name: &str) -> anyhow::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "UPDATE climbs SET name = ?3 WHERE id = ?1 AND user_id = ?2",
            params![id, user_id, name],
        )?;
        Ok(n > 0)
    }

    pub fn get_stats(&self, user_id: i64) -> anyhow::Result<Stats> {
        let conn = self.conn.lock().unwrap();
        let total_climbs: i64 = conn.query_row(
            "SELECT COUNT(*) FROM climbs WHERE user_id = ?1", params![user_id], |r| r.get(0))?;
        let total_attempts: i64 = conn.query_row(
            "SELECT COUNT(*) FROM attempts a JOIN climbs c ON a.climb_id = c.id WHERE c.user_id = ?1",
            params![user_id], |r| r.get(0))?;
        let total_gain: f64 = conn.query_row(
            "SELECT COALESCE(SUM(c.gain * sub.cnt), 0)
             FROM climbs c JOIN (SELECT climb_id, COUNT(*) cnt FROM attempts GROUP BY climb_id) sub ON sub.climb_id = c.id
             WHERE c.user_id = ?1",
            params![user_id], |r| r.get(0))?;
        let highest: f64 = conn.query_row(
            "SELECT COALESCE(MAX(end_ele), 0) FROM climbs WHERE user_id = ?1", params![user_id], |r| r.get(0))?;
        let steepest: f64 = conn.query_row(
            "SELECT COALESCE(MAX(gradient), 0) FROM climbs WHERE user_id = ?1", params![user_id], |r| r.get(0))?;

        let most: (Option<String>, i64) = conn.query_row(
            "SELECT c.name, COUNT(a.id) cnt FROM climbs c JOIN attempts a ON a.climb_id = c.id
             WHERE c.user_id = ?1 GROUP BY c.id ORDER BY cnt DESC LIMIT 1",
            params![user_id], |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap_or((None, 0));

        Ok(Stats {
            total_climbs, total_attempts, total_gain_m: total_gain,
            highest_climb_m: highest, steepest_gradient: steepest,
            most_ridden_climb: most.0, most_ridden_count: most.1,
        })
    }

    pub fn clear_user_data(&self, user_id: i64) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM climbs WHERE user_id = ?1", params![user_id])?;
        conn.execute("DELETE FROM synced_activities WHERE user_id = ?1", params![user_id])?;
        Ok(())
    }

    // ── Strava ──

    pub fn save_strava_tokens(
        &self, user_id: i64, access_token: &str, refresh_token: &str,
        expires_at: i64, athlete_id: i64, athlete_name: Option<&str>,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO strava_tokens (user_id, access_token, refresh_token, expires_at, athlete_id, athlete_name)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![user_id, access_token, refresh_token, expires_at, athlete_id, athlete_name],
        )?;
        Ok(())
    }

    pub fn get_strava_tokens(&self, user_id: i64) -> anyhow::Result<Option<StravaTokens>> {
        let conn = self.conn.lock().unwrap();
        let result = conn.query_row(
            "SELECT access_token, refresh_token, expires_at, athlete_id, athlete_name FROM strava_tokens WHERE user_id = ?1",
            params![user_id],
            |row| Ok(StravaTokens {
                access_token: row.get(0)?, refresh_token: row.get(1)?,
                expires_at: row.get(2)?, athlete_id: row.get(3)?, athlete_name: row.get(4)?,
            }),
        );
        match result {
            Ok(t) => Ok(Some(t)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn get_strava_tokens_by_athlete(&self, athlete_id: i64) -> anyhow::Result<Option<(i64, StravaTokens)>> {
        let conn = self.conn.lock().unwrap();
        let result = conn.query_row(
            "SELECT user_id, access_token, refresh_token, expires_at, athlete_id, athlete_name
             FROM strava_tokens WHERE athlete_id = ?1",
            params![athlete_id],
            |row| Ok((row.get(0)?, StravaTokens {
                access_token: row.get(1)?, refresh_token: row.get(2)?,
                expires_at: row.get(3)?, athlete_id: row.get(4)?, athlete_name: row.get(5)?,
            })),
        );
        match result {
            Ok(t) => Ok(Some(t)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn delete_strava_tokens(&self, user_id: i64) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM strava_tokens WHERE user_id = ?1", params![user_id])?;
        conn.execute("DELETE FROM synced_activities WHERE user_id = ?1", params![user_id])?;
        Ok(())
    }

    pub fn is_activity_synced(&self, user_id: i64, strava_id: i64) -> anyhow::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM synced_activities WHERE user_id = ?1 AND strava_id = ?2",
            params![user_id, strava_id], |r| r.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn mark_activity_synced(&self, user_id: i64, strava_id: i64) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO synced_activities (user_id, strava_id) VALUES (?1, ?2)",
            params![user_id, strava_id],
        )?;
        Ok(())
    }
}

fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let r = 6371.0;
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlon / 2.0).sin().powi(2);
    r * 2.0 * a.sqrt().asin()
}
