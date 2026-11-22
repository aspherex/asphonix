param([uint32]$ProcId, [int]$Top = 25)
Add-Type -TypeDefinition @'
using System; using System.Collections.Generic; using System.Runtime.InteropServices; using System.Text;
public static class Vmm {
  [StructLayout(LayoutKind.Sequential)] struct MBI { public IntPtr Base, AllocBase; public uint AllocProt; public IntPtr Size; public uint State, Prot, Type; }
  [StructLayout(LayoutKind.Sequential)] struct WSEX { public IntPtr Va; public UIntPtr Attr; }
  [DllImport("kernel32.dll")] static extern IntPtr OpenProcess(uint a, bool i, uint pid);
  [DllImport("kernel32.dll")] static extern IntPtr VirtualQueryEx(IntPtr h, IntPtr a, out MBI m, IntPtr len);
  [DllImport("psapi.dll")] static extern bool QueryWorkingSetEx(IntPtr h, [In, Out] WSEX[] b, int cb);
  [DllImport("psapi.dll", CharSet = CharSet.Unicode)] static extern uint GetMappedFileName(IntPtr h, IntPtr a, StringBuilder sb, uint n);
  public static string Run(uint pid, int top) {
    IntPtr h = OpenProcess(0x1000 | 0x0400 | 0x0010, false, pid);
    var sum = new Dictionary<string, long>(); var big = new List<string>();
    long addr = 0; MBI m; long total = 0;
    while (VirtualQueryEx(h, (IntPtr)addr, out m, (IntPtr)Marshal.SizeOf(typeof(MBI))) != IntPtr.Zero) {
      long size = (long)m.Size; long b = (long)m.Base;
      if (m.State == 0x1000) {
        string kind = m.Type == 0x1000000 ? "IMAGE" : m.Type == 0x40000 ? "MAPPED" : "PRIVATE";
        string name = "";
        if (m.Type != 0x20000) { var sb = new StringBuilder(260); if (GetMappedFileName(h, m.Base, sb, 260) > 0) name = System.IO.Path.GetFileName(sb.ToString()); }
        long pages = size / 4096; long priv = 0;
        for (long off = 0; off < pages; off += 4096) {
          int n = (int)Math.Min(4096, pages - off); var arr = new WSEX[n];
          for (int i = 0; i < n; i++) arr[i].Va = (IntPtr)(b + (off + i) * 4096);
          if (QueryWorkingSetEx(h, arr, n * IntPtr.Size * 2)) for (int i = 0; i < n; i++) { ulong a = (ulong)arr[i].Attr; bool valid = (a & 1) != 0; bool shared = ((a >> 15) & 1) != 0; if (valid && !shared) priv++; }
        }
        long pb = priv * 4096;
        if (pb > 0) {
          string key = kind + (name != "" ? " " + name : "");
          if (kind == "PRIVATE") key = "PRIVATE";
          if (kind == "IMAGE") key = "IMAGE " + name;
          sum[key] = (sum.ContainsKey(key) ? sum[key] : 0) + pb; total += pb;
          if (kind != "IMAGE" && pb >= 512 * 1024) big.Add(string.Format("{0} 0x{1:X} size={2:F1}MB privWS={3:F1}MB prot=0x{4:X} {5}", kind, b, size / 1048576.0, pb / 1048576.0, m.Prot, name));
        }
      }
      addr = (long)m.Base + size; if (addr <= 0) break;
    }
    var sbo = new StringBuilder(); sbo.AppendLine(string.Format("TOTAL private WS = {0:F1} MB", total / 1048576.0));
    var l = new List<KeyValuePair<string, long>>(sum); l.Sort((x, y) => y.Value.CompareTo(x.Value));
    int c = 0; foreach (var kv in l) { if (c++ >= top) break; sbo.AppendLine(string.Format("{0,8:F1} MB  {1}", kv.Value / 1048576.0, kv.Key)); }
    sbo.AppendLine("-- big non-image regions:"); foreach (var s in big) sbo.AppendLine(s);
    return sbo.ToString();
  }
}
'@
[Vmm]::Run($ProcId, $Top)
