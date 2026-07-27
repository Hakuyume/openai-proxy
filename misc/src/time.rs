use rand::RngExt;
use std::pin::Pin;
use std::time::Duration;

pub struct Interval {
    sleep: Pin<Box<tokio::time::Sleep>>,
    period: Duration,
}

pub fn interval(period: Duration) -> Interval {
    let now = tokio::time::Instant::now();
    Interval {
        sleep: Box::pin(tokio::time::sleep_until(
            now + rand::rng().random_range(Duration::default()..period * 2 / 5),
        )),
        period,
    }
}

impl Interval {
    pub async fn tick(&mut self) {
        self.sleep.as_mut().await;
        let mut deadline = self.sleep.deadline();
        let now = tokio::time::Instant::now();
        while deadline <= now {
            deadline += rand::rng().random_range(self.period * 4 / 5..self.period * 6 / 5);
        }
        self.sleep.as_mut().reset(deadline);
    }
}
