//! LRU-кэш тайлов с бюджетом памяти.
//!
//! Каждая запись имеет «стоимость» в байтах. Когда сумма превышает бюджет,
//! вытесняются наименее недавно использованные записи. Бюджет настраивается —
//! это ключевая защита памяти на слабых ПК.

use std::collections::HashMap;
use std::hash::Hash;

pub struct TileCache<K, V> {
    budget_bytes: usize,
    total_bytes: usize,
    entries: HashMap<K, Entry<V>>,
    /// Порядок использования: front — самый старый, back — самый свежий.
    order: Vec<K>,
}

struct Entry<V> {
    value: V,
    cost: usize,
}

impl<K: Eq + Hash + Clone, V> TileCache<K, V> {
    pub fn new(budget_bytes: usize) -> Self {
        TileCache {
            budget_bytes,
            total_bytes: 0,
            entries: HashMap::new(),
            order: Vec::new(),
        }
    }

    pub fn insert(&mut self, key: K, value: V, cost: usize) {
        if let Some(old) = self.entries.remove(&key) {
            self.total_bytes -= old.cost;
            self.order.retain(|k| k != &key);
        }
        self.entries.insert(key.clone(), Entry { value, cost });
        self.total_bytes += cost;
        self.order.push(key);
        self.evict_to_budget();
    }

    pub fn get(&mut self, key: &K) -> Option<&V> {
        if self.entries.contains_key(key) {
            self.touch(key);
            self.entries.get(key).map(|e| &e.value)
        } else {
            None
        }
    }

    /// Помечает ключ как самый свежий (перемещает в конец `order`).
    fn touch(&mut self, key: &K) {
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            let k = self.order.remove(pos);
            self.order.push(k);
        }
    }

    /// Вытесняет самые старые записи, пока сумма превышает бюджет.
    fn evict_to_budget(&mut self) {
        while self.total_bytes > self.budget_bytes && !self.order.is_empty() {
            let oldest = self.order.remove(0);
            if let Some(entry) = self.entries.remove(&oldest) {
                self.total_bytes -= entry.cost;
            }
        }
    }

    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_and_returns_a_value_within_budget() {
        let mut cache: TileCache<u32, &str> = TileCache::new(1000);
        cache.insert(1, "a", 100);
        assert_eq!(cache.get(&1), Some(&"a"));
        assert_eq!(cache.total_bytes(), 100);
    }

    #[test]
    fn evicts_least_recently_used_when_over_budget() {
        let mut cache: TileCache<u32, &str> = TileCache::new(250);
        cache.insert(1, "a", 100);
        cache.insert(2, "b", 100);
        cache.insert(3, "c", 100); // сумма 300 > 250 → вытесняем самый старый (1)

        assert_eq!(cache.get(&1), None);
        assert_eq!(cache.get(&2), Some(&"b"));
        assert_eq!(cache.get(&3), Some(&"c"));
        assert_eq!(cache.total_bytes(), 200);
    }

    #[test]
    fn get_refreshes_recency_so_touched_entry_survives() {
        let mut cache: TileCache<u32, &str> = TileCache::new(250);
        cache.insert(1, "a", 100);
        cache.insert(2, "b", 100);
        // Трогаем 1 → теперь самый старый — 2.
        assert_eq!(cache.get(&1), Some(&"a"));
        cache.insert(3, "c", 100); // вытесняем 2, а не 1

        assert_eq!(cache.get(&1), Some(&"a"));
        assert_eq!(cache.get(&2), None);
        assert_eq!(cache.get(&3), Some(&"c"));
    }

    #[test]
    fn reinsert_same_key_updates_value_without_double_counting() {
        let mut cache: TileCache<u32, &str> = TileCache::new(1000);
        cache.insert(1, "a", 100);
        cache.insert(1, "b", 150);
        assert_eq!(cache.get(&1), Some(&"b"));
        assert_eq!(cache.total_bytes(), 150);
        assert_eq!(cache.len(), 1);
    }
}
