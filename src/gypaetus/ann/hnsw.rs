use rand::prelude::ThreadRng;
use rand::Rng;
use std::borrow::Borrow;
use std::collections::BinaryHeap;
use std::collections::HashSet;
use std::fs::File;
use std::path::Path;

use super::super::util::error::GyResult;
use super::{AnnIndex, Create, Metric, Neighbor};

impl Metric<Vec<f32>> for Vec<f32> {
    fn distance(&self, b: &Vec<f32>) -> f32 {
        self.iter()
            .zip(b.iter())
            .map(|(&a1, &b1)| (a1 - b1).powi(2))
            .sum::<f32>()
            .sqrt()
    }
}

impl Create for Vec<f32> {
    fn create() -> Self {
        Vec::new()
    }
}

#[derive(Default)]
struct Node<V> {
    level: usize,
    neighbors: Vec<Vec<usize>>, //layer --> vec
    p: V,
}

impl<V> Node<V> {
    fn get_neighbors(&self, level: usize) -> Option<impl Iterator<Item = usize> + '_> {
        let x = self.neighbors.get(level)?;
        Some(x.iter().cloned())
    }
}

#[warn(non_snake_case)]
pub struct HNSW<V> {
    enter_point: usize,
    max_layer: usize,
    ef_construction: usize,
    M: usize,
    M0: usize,
    n_items: usize,
    rng: ThreadRng,
    level_mut: f64,
    nodes: Vec<Node<V>>,
    // current_id: usize,
}

impl<V> AnnIndex<V> for HNSW<V>
where
    V: Metric<V> + Create,
{
    //插入
    fn insert(&mut self, q: V, id: usize) {
        let cur_level = self.get_random_level();
        let ep_id = self.enter_point;
        let current_max_layer = self.get_node(ep_id).level;
        let new_id = id;

        //起始点
        let mut ep = Neighbor {
            id: self.enter_point,
            d: self.get_node(ep_id).p.borrow().distance(&q),
        };
        let mut changed = true;
        //那么从当前图的从最高层逐层往下寻找直至节点的层数+1停止，寻找到离data_point最近的节点，作为下面一层寻找的起始点
        for level in (cur_level..current_max_layer).rev() {
            changed = true;
            while changed {
                changed = false;
                for i in self.get_neighbors_nodes(ep.id, level).unwrap() {
                    let d = self.get_node(ep_id).p.borrow().distance(&q); //distance(self.get_node(ep_id).p.borrow(), &q);
                    if d < ep.d {
                        ep.id = i;
                        ep.d = d;
                        changed = true;
                    }
                }
            }
        }

        let new_node = Node {
            level: cur_level,
            neighbors: vec![Vec::new(); cur_level],
            p: q,
        };
        self.nodes.push(new_node);
        //从curlevel依次开始往下，每一层寻找离data_point最接近的ef_construction_（构建HNSW是可指定）个节点构成候选集
        for level in (0..core::cmp::min(cur_level, current_max_layer)).rev() {
            //在每层选择data_point最接近的ef_construction_（构建HNSW是可指定）个节点构成候选集
            let candidates = self.search_at_layer(self.get_node(new_id).p.borrow(), ep, level);
            //连接邻居?
            self.connect_neighbor(new_id, candidates, level);
        }

        self.n_items += 1;

        if cur_level > self.max_layer {
            self.max_layer = cur_level;
            self.enter_point = new_id;
        }
        //  new_id
    }

    fn search(&self, q: &V, K: usize) -> Vec<Neighbor> {
        let current_max_layer = self.max_layer;
        let mut ep = Neighbor {
            id: self.enter_point,
            d: self.get_node(self.enter_point).p.borrow().distance(&q), //distance(self.get_node(self.enter_point).p.borrow(), &q),
        };
        let mut changed = true;
        for level in (0..current_max_layer).rev() {
            changed = true;
            while changed {
                changed = false;
                if let Some(x) = self.get_neighbors_nodes(ep.id, level) {
                    for i in x {
                        let d = self.get_node(self.enter_point).p.borrow().distance(&q); // distance(self.get_node(self.enter_point).p.borrow(), &q);
                        if d < ep.d {
                            ep.id = i;
                            ep.d = d;
                            changed = true;
                        }
                    }
                }
            }
        }
        let mut x = self.search_at_layer(&q, ep, 0);
        while x.len() > K {
            x.pop();
        }
        x.into_sorted_vec()
    }
}

impl<V> HNSW<V>
where
    V: Metric<V> + Create,
{
    pub fn new(M: usize) -> HNSW<V> {
        Self {
            enter_point: 0,
            max_layer: 0,
            ef_construction: 400,
            rng: rand::thread_rng(),
            level_mut: 1f64 / ((M as f64).ln()),
            nodes: vec![Node {
                level: 0,
                neighbors: Vec::new(),
                p: V::create(),
            }],
            M: M,
            M0: M * 2,
            //   current_id: 0,
            n_items: 1,
        }
    }

    // #[warn(non_snake_case)]
    // fn load(&self, filename: &Path) -> GyResult<HNSW<V>> {
    //     let mut file = File::open(filename).unwrap();
    //     let mut r = ReadDisk::new(file);
    //     let M = r.read_usize()?;
    //     let M0 = r.read_usize()?;
    //     let ef_construction = r.read_usize()?;
    //     let level_mut = r.read_f64()?;
    //     let max_layer = r.read_usize()?;
    //     let enter_point = r.read_usize()?;
    //     let node_len = r.read_usize()?;
    //     let mut nodes: Vec<Node<V>> = Vec::with_capacity(node_len);
    //     for _ in 0..node_len {
    //         let p = r.read_vec_f32()?;
    //         let level = r.read_usize()?;
    //         let neighbor_len = r.read_usize()?;
    //         let mut neighbors: Vec<Vec<usize>> = Vec::with_capacity(neighbor_len);
    //         for _ in 0..neighbor_len {
    //             neighbors.push(r.read_vec_usize()?);
    //         }
    //         nodes.push(Node {
    //             level: level,
    //             neighbors: neighbors,
    //             p: V::create(),
    //         });
    //     }
    //     Ok(HNSW {
    //         enter_point: enter_point,
    //         max_layer: max_layer,
    //         ef_construction: ef_construction,
    //         M: M,
    //         M0: M0,
    //         n_items: 0,
    //         rng: rand::thread_rng(),
    //         level_mut: level_mut,
    //         nodes: nodes,
    //     })
    // }

    // fn save(&self, filename: &Path) -> GyResult<()> {
    //     let mut file = File::create(filename).unwrap();
    //     let mut w = WriteDisk::new(file);
    //     w.write_usize(self.M)?;
    //     w.write_usize(self.M0)?;
    //     w.write_usize(self.ef_construction)?;
    //     w.write_f64(self.level_mut)?;
    //     w.write_usize(self.max_layer)?;
    //     w.write_usize(self.enter_point)?;
    //     w.write_usize(self.nodes.len())?;

    //     for n in self.nodes.iter() {
    //         w.write_vec_f32(&n.p)?;
    //         w.write_usize(n.level)?;
    //         w.write_usize(n.neighbors.len())?;
    //         for x in n.neighbors.iter() {
    //             w.write_vec_usize(x)?;
    //         }
    //     }
    //     Ok(())
    // }

    fn save_to_buffer(&self, filename: &Path) -> GyResult<()> {
        Ok(())
    }

    fn print(&self) {
        for x in self.nodes.iter() {
            println!("level:{:?},{:?}", x.level, x.neighbors);
        }
    }

    fn get_random_level(&mut self) -> usize {
        let x: f64 = self.rng.gen();
        ((-(x * self.level_mut).ln()).floor()) as usize
    }

    fn get_node(&self, x: usize) -> &Node<V> {
        self.nodes.get(x).expect("get node fail")
    }

    fn get_node_mut(&mut self, x: usize) -> &mut Node<V> {
        self.nodes.get_mut(x).expect("get mut node fail")
    }

    //连接邻居
    fn connect_neighbor(&mut self, cur_id: usize, candidates: BinaryHeap<Neighbor>, level: usize) {
        let maxl = if level == 0 { self.M0 } else { self.M };
        let selected_neighbors = &mut self.get_node_mut(cur_id).neighbors[level]; //vec![0usize; candidates.len()]; // self.get_node_mut(cur_id); //vec![0usize; candidates.len()];
        let sort_neighbors = candidates.into_sorted_vec();
        for x in sort_neighbors.iter() {
            selected_neighbors.push(x.id);
        }
        selected_neighbors.reverse();
        //检查cur_id 的邻居的 邻居 是否超标
        for n in sort_neighbors.iter() {
            let l = {
                let node = self.get_node_mut(n.id);
                if node.neighbors.len() < level + 1 {
                    for _ in node.neighbors.len()..=level {
                        node.neighbors.push(Vec::with_capacity(maxl));
                    }
                }
                let x = node.neighbors.get_mut(level).unwrap();
                //将cur_id插入到 邻居的 neighbors中
                x.push(cur_id);
                x.len()
            };
            //检查每个neighbors的连接数，如果大于maxl，则需要缩减连接到最近邻的maxl个
            if l > maxl {
                let mut result_set: BinaryHeap<Neighbor> = BinaryHeap::with_capacity(maxl);

                let p = self.get_node(n.id).p.borrow();
                self.get_neighbors_nodes(n.id, level)
                    .unwrap()
                    .for_each(|x| {
                        result_set.push(Neighbor {
                            id: x,
                            d: -p.distance(self.get_node(x).p.borrow()), //distance(p, self.get_node(x).p.borrow()),
                        });
                    });

                self.get_neighbors_by_heuristic_closest_frist(&mut result_set, self.M);
                let neighbors = self.get_node_mut(n.id).neighbors.get_mut(level).unwrap();
                neighbors.clear();
                for x in result_set.iter() {
                    neighbors.push(x.id);
                }
                neighbors.reverse();
            }
        }
    }

    fn get_neighbors_nodes(
        &self,
        n: usize,
        level: usize,
    ) -> Option<impl Iterator<Item = usize> + '_> {
        self.get_node(n).get_neighbors(level)
    }

    // 返回 result 从远到近
    fn search_at_layer(&self, q: &V, ep: Neighbor, level: usize) -> BinaryHeap<Neighbor> {
        let mut visited_set: HashSet<usize> = HashSet::new();
        let mut candidates: BinaryHeap<Neighbor> =
            BinaryHeap::with_capacity(self.ef_construction * 3);
        let mut results: BinaryHeap<Neighbor> = BinaryHeap::new();

        candidates.push(Neighbor {
            id: ep.id,
            d: -ep.d,
        });
        visited_set.insert(ep.id);
        results.push(ep);

        // 从candidates中选择距离查询点最近的点c
        while let Some(c) = candidates.pop() {
            let d = results.peek().unwrap();
            // 从candidates中选择距离查询点最近的点c，和results中距离查询点最远的点d进行比较，
            // 如果c和查询点q的距离大于d和查询点q的距离，则结束查询
            if -c.d > d.d {
                break;
            }
            if self.get_node(c.id).neighbors.len() < level + 1 {
                continue;
            }
            // 查询c的所有邻居e，如果e已经在visitedset中存在则跳过，不存在则加入visitedset
            // 把比d和q距离更近的e加入candidates、results中，如果results未满，
            // 则把所有的e都加入candidates、results
            // 如果results已满，则弹出和q距离最远的点
            self.get_neighbors_nodes(c.id, level)
                .unwrap()
                .for_each(|n| {
                    //如果e已经在visitedset中存在则跳过，
                    if visited_set.contains(&n) {
                        return;
                    }
                    //不存在则加入visitedset
                    visited_set.insert(n);
                    let dist = q.distance(self.nodes.get(n).unwrap().p.borrow()); //   distance(q, self.nodes.get(n).unwrap().p.borrow());
                    let top_d = results.peek().unwrap();
                    //如果results未满，则把所有的e都加入candidates、results

                    if results.len() < self.ef_construction {
                        results.push(Neighbor { id: n, d: dist });
                        candidates.push(Neighbor { id: n, d: -dist });
                    } else if dist < top_d.d {
                        // 如果results已满，则弹出和q距离最远的点
                        results.pop();
                        results.push(Neighbor { id: n, d: dist });
                        candidates.push(Neighbor { id: n, d: -dist });
                    }
                });
        }
        results
    }

    fn get_neighbors_by_heuristic_closest_frist(&mut self, w: &mut BinaryHeap<Neighbor>, M: usize) {
        if w.len() <= M {
            return;
        }
        let mut temp_list: BinaryHeap<Neighbor> = BinaryHeap::with_capacity(w.len());
        let mut result: BinaryHeap<Neighbor> = BinaryHeap::new();
        while w.len() > 0 {
            if result.len() >= M {
                break;
            }
            //从w中提取q得最近邻 e
            let e = w.pop().unwrap();
            let dist = -e.d;
            //如果e和q的距离比e和R中的其中一个元素的距离更小，就把e加入到result中
            if result
                .iter()
                .map(|r| self.nodes[r.id].p.borrow().distance(&self.nodes[e.id].p)) //distance(self.nodes[r.id].p.borrow(), &self.nodes[e.id].p)
                .any(|x| dist < x)
            {
                result.push(e);
            } else {
                temp_list.push(e);
            }
        }
        while result.len() < M {
            if let Some(e) = temp_list.pop() {
                result.push(e);
            } else {
                break;
            }
        }
        result.iter().for_each(|item| {
            w.push(Neighbor {
                id: item.id,
                d: -item.d,
            })
        });
    }

    // 探索式寻找最近邻
    // 在W中选择q最近邻的M个点作为neighbors双向连接起来 启发式算法
    // https://www.ryanligod.com/2019/07/23/2019-07-23%20%E5%85%B3%E4%BA%8E%20HNSW%20%E5%90%AF%E5%8F%91%E5%BC%8F%E7%AE%97%E6%B3%95%E7%9A%84%E4%B8%80%E4%BA%9B%E7%9C%8B%E6%B3%95/
    // 启发式选择的目的不是为了解决图的全局连通性，而是为了有一条“高速公路”可以到另一个区域
    // 候选元素队列不为空且结果数量少于M时，在W中选择q最近邻e
    // 如果e和q的距离比e和R中的其中一个元素的距离更小，就把e加入到R中，否则就把e加入Wd（丢弃）
    fn get_neighbors_by_heuristic(&mut self, candidates: &mut BinaryHeap<Neighbor>, M: usize) {
        if candidates.len() <= M {
            return;
        }
        let mut temp_list: BinaryHeap<Neighbor> = BinaryHeap::with_capacity(candidates.len());
        let mut result: BinaryHeap<Neighbor> = BinaryHeap::new();
        let mut w: BinaryHeap<Neighbor> = BinaryHeap::with_capacity(candidates.len());
        while let Some(e) = candidates.pop() {
            w.push(Neighbor { id: e.id, d: -e.d });
        }

        while w.len() > 0 {
            if result.len() >= M {
                break;
            }
            //从w中提取q得最近邻 e
            let e = w.pop().unwrap();
            let dist = -e.d;
            //如果e和q的距离比e和R中的其中一个元素的距离更小，就把e加入到result中
            if result
                .iter()
                .map(|r| self.nodes[r.id].p.borrow().distance(&self.nodes[e.id].p)) //distance(self.nodes[r.id].p.borrow(), &self.nodes[e.id].p)
                .any(|x| dist < x)
            {
                result.push(e);
            } else {
                temp_list.push(e);
            }
        }
        while result.len() < M {
            if let Some(e) = temp_list.pop() {
                result.push(e);
            } else {
                break;
            }
        }
        result.iter().for_each(|item| {
            candidates.push(Neighbor {
                id: item.id,
                d: -item.d,
            })
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{thread_rng, Rng};
    use std::collections::HashMap;

    #[test]
    fn test_rng() {
        let mut rng = rand::thread_rng();
        let x: f64 = rng.gen();
        println!("x = {x}");
        let x: f64 = rng.gen();
        println!("x = {x}");
        let x: f64 = rng.gen();
        println!("x = {x}");
    }

    #[test]
    fn test_hnsw_rand_level() {
        // let mut hnsw = HNSW::new(32);
        // println!("{}", hnsw.level_mut);
        // for x in 0..100 {
        //     let x1 = hnsw.get_random_level();
        //     println!("x1 = {x1}");
        // }
    }

    #[test]
    fn test_hnsw_binary_heap() {
        let mut heap: BinaryHeap<Neighbor> = BinaryHeap::new();

        heap.push(Neighbor { id: 0, d: 10.0 });
        heap.push(Neighbor { id: 2, d: 9.0 });
        heap.push(Neighbor { id: 1, d: 15.0 });
        println!("{:?}", heap.into_sorted_vec()); //
                                                  //  println!("{:?}", heap.peek()); //
                                                  //   println!("{:?}", heap);
    }

    #[test]
    fn test_hnsw_search() {
        let mut hnsw = HNSW::<Vec<f32>>::new(32);

        let features = [
            &[0.0, 0.0, 0.0, 1.0],
            &[0.0, 0.0, 1.0, 0.0],
            &[0.0, 1.0, 0.0, 0.0],
            &[1.0, 0.0, 0.0, 0.0],
            &[0.0, 0.0, 1.0, 1.0],
            &[0.0, 1.0, 1.0, 0.0],
            &[1.0, 1.0, 0.0, 0.0],
            &[1.0, 0.0, 0.0, 1.0],
        ];

        //let mut x: HashMap<usize, usize> = HashMap::new();
        // for _ in 0..=10000 {
        //     let l = hnsw.get_random_level();
        //     let i = x.entry(l).or_insert(0);
        //     *i = *i + 1;
        // }
        // println!("{:?}", x);
        let mut i = 1;
        for &feature in &features {
            hnsw.insert(feature.to_vec(), i);
            i += 1;
        }

        hnsw.print();

        let neighbors = hnsw.search(&[0.0f32, 0.0, 1.0, 0.0][..].to_vec(), 4);
        println!("{:?}", neighbors);
    }
}
