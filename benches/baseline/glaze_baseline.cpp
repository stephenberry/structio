// Generates the benchmark document, then times Glaze on it.
// The Rust side reads the same file, so the comparison is over identical bytes.
#include <chrono>
#include <cstdio>
#include <fstream>
#include <random>
#include <string>
#include <vector>
#include <algorithm>
#include "glaze/glaze.hpp"

struct test_struct {
   std::vector<std::string> testStrings{};
   std::vector<uint64_t> testUints{};
   std::vector<double> testDoubles{};
   std::vector<int64_t> testInts{};
   std::vector<bool> testBools{};
};

struct test_generator {
   std::vector<test_struct> a, b, c, d, e, f, g, h, i, j, k, l, m,
       n, o, p, q, r, s, t, u, v, w, x, y, z;
};

template <> struct glz::meta<test_struct> {
   using T = test_struct;
   static constexpr auto value =
       object(&T::testStrings, &T::testUints, &T::testDoubles, &T::testInts, &T::testBools);
};

template <> struct glz::meta<test_generator> {
   using T = test_generator;
   static constexpr auto value = object(
       &T::a, &T::b, &T::c, &T::d, &T::e, &T::f, &T::g, &T::h, &T::i, &T::j, &T::k, &T::l, &T::m,
       &T::n, &T::o, &T::p, &T::q, &T::r, &T::s, &T::t, &T::u, &T::v, &T::w, &T::x, &T::y, &T::z);
};

static std::mt19937_64 gen{1};
static constexpr std::string_view charset{
    "!#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[]^_`abcdefghijklmnopqrstuvwxyz{|}~"};

static std::string gen_string() {
   std::normal_distribution<> len{35.0, 10.0};
   auto n = (size_t)std::max(1.0, std::abs(len(gen)));
   std::uniform_int_distribution<size_t> pick(0, charset.size() - 1);
   std::string s;
   s.reserve(n);
   for (size_t i = 0; i < n; ++i) s += charset[pick(gen)];
   return s;
}

static test_struct gen_struct() {
   test_struct t;
   std::uniform_int_distribution<size_t> count(20, 40);
   std::uniform_int_distribution<uint64_t> u(0, 1000000);
   std::uniform_int_distribution<int64_t> i(-1000000, 1000000);
   std::uniform_real_distribution<double> d(-1e6, 1e6);
   std::uniform_int_distribution<int> b(0, 1);
   size_t n = count(gen);
   for (size_t k = 0; k < n; ++k) {
      t.testStrings.push_back(gen_string());
      t.testUints.push_back(u(gen));
      t.testInts.push_back(i(gen));
      t.testDoubles.push_back(d(gen));
      t.testBools.push_back(b(gen));
   }
   return t;
}

// Keep only one vector kind populated, so a document isolates one converter.
enum class only { all, strings, uints, doubles, ints, bools, nice_doubles };

static test_generator make_data(only which) {
   gen.seed(1);
   test_generator data;
   auto fill = [&](std::vector<test_struct>& v) {
      for (int k = 0; k < 4; ++k) {
         auto t = gen_struct();
         if (which != only::all) {
            if (which != only::strings) t.testStrings.clear();
            if (which != only::uints) t.testUints.clear();
            if (which != only::doubles && which != only::nice_doubles) t.testDoubles.clear();
            if (which == only::nice_doubles)
               for (auto& d : t.testDoubles) d = double(int(d) % 1000) / 8.0;
            if (which != only::ints) t.testInts.clear();
            if (which != only::bools) t.testBools.clear();
         }
         v.push_back(std::move(t));
      }
   };
   for (auto* v : {&data.a, &data.b, &data.c, &data.d, &data.e, &data.f, &data.g, &data.h,
                   &data.i, &data.j, &data.k, &data.l, &data.m, &data.n, &data.o, &data.p,
                   &data.q, &data.r, &data.s, &data.t, &data.u, &data.v, &data.w, &data.x,
                   &data.y, &data.z})
      fill(*v);
   return data;
}

static void run(const char* label, only which, const std::string& path) {
   auto data = make_data(which);
   std::string buffer;
   if (glz::write_json(data, buffer)) { std::puts("write failed"); return; }
   { std::ofstream out(path, std::ios::binary); out << buffer; }

   const int iters = std::max<int>(20, int(40'000'000 / buffer.size()));

   test_generator dst;
   auto t0 = std::chrono::steady_clock::now();
   for (int k = 0; k < iters; ++k) {
      if (glz::read<glz::opts{.error_on_unknown_keys = false}>(dst, buffer)) {
         std::puts("read failed"); return;
      }
   }
   auto t1 = std::chrono::steady_clock::now();
   double read_s = std::chrono::duration<double>(t1 - t0).count();

   std::string out;
   out.reserve(buffer.size() * 2);
   t0 = std::chrono::steady_clock::now();
   for (int k = 0; k < iters; ++k) std::ignore = glz::write_json(dst, out);
   t1 = std::chrono::steady_clock::now();
   double write_s = std::chrono::duration<double>(t1 - t0).count();

   // Lay the same bytes out again as text, with no type in the way.
   std::string pretty;
   pretty.reserve(buffer.size() * 2);
   glz::prettify_json(buffer, pretty);
   t0 = std::chrono::steady_clock::now();
   for (int k = 0; k < iters; ++k) glz::prettify_json(buffer, pretty);
   t1 = std::chrono::steady_clock::now();
   double pretty_s = std::chrono::duration<double>(t1 - t0).count();

   // Leave the prettified form beside the document, so the Rust side can check
   // its own output against it rather than only its throughput.
   {
      std::string p = path;
      p.replace(p.find(".json"), 5, "_pretty.json");
      std::ofstream out2(p, std::ios::binary);
      out2 << pretty;
   }

   // And take the layout back out, which is the same bytes going the other way.
   // Timed over the prettified document, since that is what is being read.
   // `minify_json` pads its input and unpads it again, leaving it as it found
   // it, so one copy outside the loop is enough.
   std::string mini;
   mini.reserve(pretty.size());
   std::string src = pretty;
   glz::minify_json(src, mini);
   t0 = std::chrono::steady_clock::now();
   for (int k = 0; k < iters; ++k) glz::minify_json(src, mini);
   t1 = std::chrono::steady_clock::now();
   double minify_s = std::chrono::duration<double>(t1 - t0).count();

   double mb = double(buffer.size()) * iters / 1048576.0;
   double pretty_mb = double(pretty.size()) * iters / 1048576.0;
   std::printf(
       "%-9s %7zu B  read %8.2f MB/s   write %8.2f MB/s   pretty %8.2f MB/s   minify %8.2f MB/s  %s%s\n",
       label, buffer.size(), mb / read_s, mb / write_s, mb / pretty_s, pretty_mb / minify_s,
       out == buffer ? "" : "(ROUNDTRIP DIFFERS) ", mini == buffer ? "" : "(MINIFY DIFFERS)");
}

int main() {
   std::printf("--- glaze ---\n");
   run("mixed",   only::all,     "tmp/bench.json");
   run("strings", only::strings, "tmp/bench_strings.json");
   run("uints",   only::uints,   "tmp/bench_uints.json");
   run("doubles", only::doubles, "tmp/bench_doubles.json");
   run("ints",    only::ints,    "tmp/bench_ints.json");
   run("bools",   only::bools,   "tmp/bench_bools.json");
   // Exact short decimals (n/8), the shape that real data is full of and that
   // shortest-representation algorithms find hardest.
   run("nice-dbl", only::nice_doubles, "tmp/bench_nice_doubles.json");
   return 0;
}
