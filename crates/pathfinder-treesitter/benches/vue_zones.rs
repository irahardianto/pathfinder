use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use pathfinder_treesitter::vue_zones::{parse_vue_multizone, scan_vue_zones};

const SMALL_SFC: &[u8] = br#"<template>
  <div>Hello</div>
</template>
<script setup lang="ts">
const x = 1
</script>
<style>
.app { color: red; }
</style>"#;

const MEDIUM_SFC: &[u8] = br#"<template>
  <div class="app">
    <Header title="My App" />
    <Sidebar :items="menuItems" />
    <main class="content">
      <Dashboard v-if="showDashboard" />
      <Settings v-else-if="showSettings" />
      <Profile v-else />
    </main>
    <Footer />
  </div>
</template>
<script setup lang="ts">
import { ref, computed, watch, onMounted } from 'vue'
import Header from './Header.vue'
import Sidebar from './Sidebar.vue'
import Dashboard from './Dashboard.vue'
import Settings from './Settings.vue'
import Profile from './Profile.vue'
import Footer from './Footer.vue'

const showDashboard = ref(true)
const showSettings = ref(false)
const menuItems = ref(['home', 'settings', 'profile'])

const currentView = computed(() => {
  if (showDashboard.value) return 'dashboard'
  if (showSettings.value) return 'settings'
  return 'profile'
})

watch(currentView, (val) => {
  console.log('view changed:', val)
})

onMounted(() => {
  console.log('app mounted')
})

function navigate(view: string) {
  showDashboard.value = view === 'dashboard'
  showSettings.value = view === 'settings'
}

function toggleSidebar() {
  menuItems.value = menuItems.value.length > 0 ? [] : ['home', 'settings', 'profile']
}
</script>
<style scoped>
.app { display: flex; flex-direction: column; min-height: 100vh; }
.content { flex: 1; padding: 1rem; }
</style>"#;

fn generate_large_sfc(n_components: usize) -> Vec<u8> {
    let mut sfc = String::from("<template>\n  <div class=\"app\">\n");
    for i in 0..n_components {
        sfc.push_str(&format!("    <Widget{i:04} :data=\"items[{i}]\" />\n"));
    }
    sfc.push_str("  </div>\n</template>\n<script setup lang=\"ts\">\n");
    sfc.push_str("import { ref } from 'vue'\n");
    sfc.push_str("const items = ref([] as any[])\n");
    for i in 0..n_components {
        sfc.push_str(&format!(
            "function handler{i:04}() {{ items.value.push({i}) }}\n"
        ));
    }
    sfc.push_str("</script>\n<style scoped>\n");
    sfc.push_str(".app { display: flex; }\n");
    for i in 0..n_components {
        sfc.push_str(&format!(".widget-{i:04} {{ margin: {i}px; }}\n"));
    }
    sfc.push_str("</style>");
    sfc.into_bytes()
}

fn bench_vue_zones(c: &mut Criterion) {
    let mut group = c.benchmark_group("vue_zones");

    group.throughput(Throughput::Bytes(SMALL_SFC.len() as u64));
    group.bench_function("scan_small_sfc", |b| {
        b.iter(|| scan_vue_zones(black_box(SMALL_SFC)));
    });

    group.throughput(Throughput::Bytes(SMALL_SFC.len() as u64));
    group.bench_function("parse_small_sfc", |b| {
        b.iter(|| parse_vue_multizone(black_box(SMALL_SFC)).expect("parse"));
    });

    group.throughput(Throughput::Bytes(MEDIUM_SFC.len() as u64));
    group.bench_function("parse_medium_sfc", |b| {
        b.iter(|| parse_vue_multizone(black_box(MEDIUM_SFC)).expect("parse"));
    });

    for &n in &[20, 50, 100] {
        let large_sfc = generate_large_sfc(n);
        group.throughput(Throughput::Bytes(large_sfc.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("parse_large", n),
            &large_sfc,
            |b, sfc| {
                b.iter(|| parse_vue_multizone(black_box(sfc)).expect("parse"));
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_vue_zones);
criterion_main!(benches);
