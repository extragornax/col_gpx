use rusqlite::{params, Connection};
use std::sync::Mutex;

pub struct Db {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ClimbAttempt {
    pub id: i64,
    pub climb_id: i64,
    pub activity_date: String,
    pub activity_name: Option<String>,
    pub elapsed_seconds: Option<f64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Stats {
    pub total_climbs: i64,
    pub total_attempts: i64,
    pub total_gain_m: f64,
    pub highest_climb_m: f64,
    pub steepest_gradient: f64,
    pub most_ridden_climb: Option<String>,
    pub most_ridden_count: i64,
}

impl Db {
    pub fn open(path: &str) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000; PRAGMA foreign_keys=ON;")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn migrate(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS climbs (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
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
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                climb_id      INTEGER NOT NULL REFERENCES climbs(id) ON DELETE CASCADE,
                activity_date TEXT NOT NULL,
                activity_name TEXT,
                elapsed_seconds REAL
            );

            CREATE INDEX IF NOT EXISTS idx_climbs_loc ON climbs(lat, lon);
            CREATE INDEX IF NOT EXISTS idx_attempts_climb ON attempts(climb_id);"
        )?;
        Ok(())
    }

    pub fn find_nearby_climb(&self, lat: f64, lon: f64, radius_km: f64) -> anyhow::Result<Option<i64>> {
        let conn = self.conn.lock().unwrap();
        // Approximate bounding box (1 degree lat ≈ 111 km)
        let dlat = radius_km / 111.0;
        let dlon = radius_km / (111.0 * lat.to_radians().cos());

        let mut stmt = conn.prepare(
            "SELECT id, lat, lon FROM climbs
             WHERE lat BETWEEN ?1 AND ?2 AND lon BETWEEN ?3 AND ?4"
        )?;
        let rows: Vec<(i64, f64, f64)> = stmt.query_map(
            params![lat - dlat, lat + dlat, lon - dlon, lon + dlon],
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
        &self,
        lat: f64,
        lon: f64,
        start_ele: f64,
        end_ele: f64,
        gain: f64,
        distance_km: f64,
        gradient: f64,
        date: &str,
    ) -> anyhow::Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO climbs (lat, lon, start_ele, end_ele, gain, distance_km, gradient, first_ridden, last_ridden)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
            params![lat, lon, start_ele, end_ele, gain, distance_km, gradient, date],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn add_attempt(
        &self,
        climb_id: i64,
        date: &str,
        activity_name: Option<&str>,
        elapsed: Option<f64>,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO attempts (climb_id, activity_date, activity_name, elapsed_seconds)
             VALUES (?1, ?2, ?3, ?4)",
            params![climb_id, date, activity_name, elapsed],
        )?;
        conn.execute(
            "UPDATE climbs SET last_ridden = MAX(last_ridden, ?2) WHERE id = ?1",
            params![climb_id, date],
        )?;
        Ok(())
    }

    pub fn get_climbs(&self) -> anyhow::Result<Vec<ClimbRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT c.id, c.name, c.lat, c.lon, c.start_ele, c.end_ele, c.gain, c.distance_km,
                    c.gradient, COUNT(a.id), c.first_ridden, c.last_ridden
             FROM climbs c
             LEFT JOIN attempts a ON a.climb_id = c.id
             GROUP BY c.id
             ORDER BY c.gain DESC"
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(ClimbRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                lat: row.get(2)?,
                lon: row.get(3)?,
                start_ele: row.get(4)?,
                end_ele: row.get(5)?,
                gain: row.get(6)?,
                distance_km: row.get(7)?,
                gradient: row.get(8)?,
                times_ridden: row.get(9)?,
                first_ridden: row.get(10)?,
                last_ridden: row.get(11)?,
            })
        })?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_climb(&self, id: i64) -> anyhow::Result<Option<ClimbRecord>> {
        let conn = self.conn.lock().unwrap();
        let result = conn.query_row(
            "SELECT c.id, c.name, c.lat, c.lon, c.start_ele, c.end_ele, c.gain, c.distance_km,
                    c.gradient, COUNT(a.id), c.first_ridden, c.last_ridden
             FROM climbs c
             LEFT JOIN attempts a ON a.climb_id = c.id
             WHERE c.id = ?1
             GROUP BY c.id",
            params![id],
            |row| Ok(ClimbRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                lat: row.get(2)?,
                lon: row.get(3)?,
                start_ele: row.get(4)?,
                end_ele: row.get(5)?,
                gain: row.get(6)?,
                distance_km: row.get(7)?,
                gradient: row.get(8)?,
                times_ridden: row.get(9)?,
                first_ridden: row.get(10)?,
                last_ridden: row.get(11)?,
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
                id: row.get(0)?,
                climb_id: row.get(1)?,
                activity_date: row.get(2)?,
                activity_name: row.get(3)?,
                elapsed_seconds: row.get(4)?,
            })
        })?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn rename_climb(&self, id: i64, name: &str) -> anyhow::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "UPDATE climbs SET name = ?2 WHERE id = ?1",
            params![id, name],
        )?;
        Ok(n > 0)
    }

    pub fn get_stats(&self) -> anyhow::Result<Stats> {
        let conn = self.conn.lock().unwrap();
        let total_climbs: i64 = conn.query_row("SELECT COUNT(*) FROM climbs", [], |r| r.get(0))?;
        let total_attempts: i64 = conn.query_row("SELECT COUNT(*) FROM attempts", [], |r| r.get(0))?;
        let total_gain: f64 = conn.query_row(
            "SELECT COALESCE(SUM(c.gain * sub.cnt), 0)
             FROM climbs c JOIN (SELECT climb_id, COUNT(*) cnt FROM attempts GROUP BY climb_id) sub ON sub.climb_id = c.id",
            [], |r| r.get(0),
        )?;
        let highest: f64 = conn.query_row("SELECT COALESCE(MAX(end_ele), 0) FROM climbs", [], |r| r.get(0))?;
        let steepest: f64 = conn.query_row("SELECT COALESCE(MAX(gradient), 0) FROM climbs", [], |r| r.get(0))?;

        let most: (Option<String>, i64) = conn.query_row(
            "SELECT c.name, COUNT(a.id) cnt FROM climbs c JOIN attempts a ON a.climb_id = c.id GROUP BY c.id ORDER BY cnt DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap_or((None, 0));

        Ok(Stats {
            total_climbs,
            total_attempts,
            total_gain_m: total_gain,
            highest_climb_m: highest,
            steepest_gradient: steepest,
            most_ridden_climb: most.0,
            most_ridden_count: most.1,
        })
    }

    pub fn clear_all(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch("DELETE FROM attempts; DELETE FROM climbs;")?;
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
