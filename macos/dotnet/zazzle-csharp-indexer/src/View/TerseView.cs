// Terse behavioural view of a C# file: hides usings, logging, regions, trivial constructors and log-and-rethrow catches, and shortens declarations, keeping source line numbers.
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;
using Microsoft.CodeAnalysis.Text;

namespace ZazzleIndexer.View;

// One line of the view: a source line, possibly changed, or a note standing for the hidden lines Start to End.
public sealed record ViewLine(int Start, int End, string Text, bool Changed, bool Note);

// Builds terse views; every rule is syntactic and conservative, so behaviour stays visible.
public static class TerseView
{
    private static readonly string[] LogMethods = ["Debug", "Info", "Information", "Warn", "Warning", "Error", "Fatal", "Trace", "Verbose", "Critical"];
    private static readonly string[] TelemetryMethods = ["SetTag", "AddTag", "AddEvent", "SetStatus", "SetBaggage", "AddBaggage", "RecordException", "AddException"];
    private static readonly SyntaxKind[] Access = [SyntaxKind.PublicKeyword, SyntaxKind.PrivateKeyword, SyntaxKind.ProtectedKeyword, SyntaxKind.InternalKeyword];

    // Returns the view lines and, per step, how many places it changed.
    public static (List<ViewLine> Lines, List<(string Step, int Count)> Steps) Build(string source)
    {
        var tree = CSharpSyntaxTree.ParseText(source);
        var root = tree.GetCompilationUnitRoot();
        var plan = new Plan(tree.GetText());
        CollapseUsings(root, plan);
        CollapseConstructors(root, plan);
        CollapseRethrowCatches(root, plan);
        HideLogging(root, plan);
        HideRegions(root, plan);
        DropAccessModifiers(root, plan);
        UseVar(root, plan);
        return (plan.Render(), plan.Steps);
    }

    // Collapses each block of using directives into one line.
    private static void CollapseUsings(CompilationUnitSyntax root, Plan plan)
    {
        IEnumerable<SyntaxList<UsingDirectiveSyntax>> blocks = [root.Usings, .. root.DescendantNodes().OfType<BaseNamespaceDeclarationSyntax>().Select(n => n.Usings)];
        foreach (var usings in blocks.Where(u => u.Count > 0))
            plan.Hide(usings.First(), usings.Last(), $"{usings.Count} using(s)", "usings");
    }

    // True for a statement whose only effect is logging or tracing, with no side effects in its arguments.
    public static bool IsLogging(StatementSyntax s)
    {
        if (s is LocalDeclarationStatementSyntax { UsingKeyword.RawKind: not 0 } local)
            return local.Declaration.Variables.All(v => v.Initializer?.Value is InvocationExpressionSyntax inv && CalleeName(inv) == "StartActivity");
        if (s is not ExpressionStatementSyntax { Expression: var e } || HasSideEffects(e))
            return false;
        return e switch
        {
            InvocationExpressionSyntax { Expression: MemberAccessExpressionSyntax m } => IsLogCall(Last(m.Expression), m.Name.Identifier.Text),
            ConditionalAccessExpressionSyntax { Expression: var r, WhenNotNull: InvocationExpressionSyntax { Expression: MemberBindingExpressionSyntax b } } => IsLogCall(Last(r), b.Name.Identifier.Text),
            _ => false,
        };
    }

    // True for a call on a logger, Debug/Trace, or a tracing span.
    private static bool IsLogCall(string receiver, string method)
    {
        var r = receiver.TrimStart('_').ToLowerInvariant();
        if (receiver is "Debug" or "Trace")
            return true;
        if ((r is "log" or "logs" or "logging" || r.EndsWith("logger")) && (method.StartsWith("Log", StringComparison.Ordinal) || LogMethods.Contains(method)))
            return true;
        return (r.Contains("activity") || r.Contains("span")) && TelemetryMethods.Contains(method);
    }

    // Returns the last identifier of a receiver expression, such as `_logger` in `this._logger`.
    private static string Last(ExpressionSyntax e) => e switch
    {
        MemberAccessExpressionSyntax m => m.Name.Identifier.Text,
        IdentifierNameSyntax id => id.Identifier.Text,
        _ => e.ToString(),
    };

    // Returns the method name an invocation calls.
    private static string CalleeName(InvocationExpressionSyntax inv) => inv.Expression switch
    {
        MemberAccessExpressionSyntax m => m.Name.Identifier.Text,
        IdentifierNameSyntax id => id.Identifier.Text,
        _ => "",
    };

    // True when an expression assigns, increments, awaits or creates a lambda that might.
    private static bool HasSideEffects(ExpressionSyntax e) => e.DescendantNodes().Any(n =>
        n is AssignmentExpressionSyntax or AwaitExpressionSyntax
        || n is PrefixUnaryExpressionSyntax { RawKind: (int)SyntaxKind.PreIncrementExpression or (int)SyntaxKind.PreDecrementExpression }
        || n is PostfixUnaryExpressionSyntax { RawKind: (int)SyntaxKind.PostIncrementExpression or (int)SyntaxKind.PostDecrementExpression });

    // Hides logging statements, and `if (logger.IsEnabled(..))` blocks that only log.
    private static void HideLogging(CompilationUnitSyntax root, Plan plan)
    {
        foreach (var s in root.DescendantNodes().OfType<StatementSyntax>())
        {
            if (IsLogging(s) && s.Parent is BlockSyntax or SwitchSectionSyntax)
                plan.Hide(s, s, null, "log statements");
            else if (s is IfStatementSyntax { Else: null } i && i.Condition.ToString().Contains("IsEnabled") && Body(i.Statement).All(IsLogging))
                plan.Hide(s, s, null, "log statements");
        }
    }

    // Returns the statements of a block, or the statement itself.
    private static IEnumerable<StatementSyntax> Body(StatementSyntax s) => s is BlockSyntax b ? b.Statements : [s];

    // True for a statement that only stores a value: `x = y;`, `this.x = y;` or `x = y ?? throw ..;`.
    private static bool IsStore(StatementSyntax s) =>
        s is ExpressionStatementSyntax { Expression: AssignmentExpressionSyntax { RawKind: (int)SyntaxKind.SimpleAssignmentExpression, Right: var r } }
        && (r is IdentifierNameSyntax || r is BinaryExpressionSyntax { RawKind: (int)SyntaxKind.CoalesceExpression, Right: ThrowExpressionSyntax });

    // Collapses constructors that only store their parameters into one line naming the parameters.
    private static void CollapseConstructors(CompilationUnitSyntax root, Plan plan)
    {
        foreach (var c in root.DescendantNodes().OfType<ConstructorDeclarationSyntax>())
        {
            var stores = c.Body is { } b ? b.Statements.All(IsStore) : c.ExpressionBody?.Expression is AssignmentExpressionSyntax;
            if (!stores || c.Modifiers.Any(SyntaxKind.StaticKeyword))
                continue;
            var init = c.Initializer is { } i ? $" : {i.ThisOrBaseKeyword.Text}({i.ArgumentList.Arguments.Count} args)" : "";
            var what = c.Body is { Statements.Count: 0 } ? "empty" : "only assigns fields";
            plan.Hide(c, c, $"ctor({string.Join(", ", c.ParameterList.Parameters.Select(p => $"{p.Type} {p.Identifier.Text}"))}){init}: {what}", "constructors");
        }
    }

    // Collapses catch clauses that only log and rethrow.
    private static void CollapseRethrowCatches(CompilationUnitSyntax root, Plan plan)
    {
        foreach (var c in root.DescendantNodes().OfType<CatchClauseSyntax>())
        {
            var body = c.Block.Statements;
            var rethrows = body.Count > 0 && body.Last() is ThrowStatementSyntax { Expression: null } && body.SkipLast(1).All(IsLogging);
            if (rethrows && c.Filter is null)
                plan.Hide(c, c, $"catch ({c.Declaration?.Type.ToString() ?? "all"}): logs and rethrows", "log-and-rethrow catches");
        }
    }

    // Hides #region and #endregion lines.
    private static void HideRegions(CompilationUnitSyntax root, Plan plan)
    {
        foreach (var t in root.DescendantTrivia(descendIntoTrivia: true))
            if (t.IsKind(SyntaxKind.RegionDirectiveTrivia) || t.IsKind(SyntaxKind.EndRegionDirectiveTrivia))
                plan.HideLine(t.GetLocation().GetLineSpan().StartLinePosition.Line, "regions");
    }

    // Removes public, private, protected and internal from declarations.
    private static void DropAccessModifiers(CompilationUnitSyntax root, Plan plan)
    {
        foreach (var t in root.DescendantTokens().Where(t => Access.Any(k => t.IsKind(k)) && t.Parent is MemberDeclarationSyntax or AccessorDeclarationSyntax))
            plan.Change(TextSpan.FromBounds(t.Span.Start, t.TrailingTrivia.Span.End), "", "access modifiers");
    }

    // Uses var where a local's type is spelled out on the right: `new T(..)`, `(T)x` or `x as T`.
    private static void UseVar(CompilationUnitSyntax root, Plan plan)
    {
        foreach (var d in root.DescendantNodes().OfType<LocalDeclarationStatementSyntax>())
        {
            var decl = d.Declaration;
            if (d.IsConst || decl.Type.IsVar || decl.Variables.Count != 1 || decl.Variables[0].Initializer?.Value is not { } value)
                continue;
            var spelled = value switch
            {
                ObjectCreationExpressionSyntax o => o.Type,
                CastExpressionSyntax c => c.Type,
                BinaryExpressionSyntax { RawKind: (int)SyntaxKind.AsExpression, Right: TypeSyntax t } => t,
                _ => null,
            };
            if (spelled is not null && spelled.ToString() == decl.Type.ToString())
                plan.Change(decl.Type.Span, "var", "var");
        }
    }

    // Collects hidden ranges and single-line text changes, then renders the view.
    private sealed class Plan(SourceText text)
    {
        private readonly List<(int Start, int End, string? Note)> _hidden = [];
        private readonly List<TextChange> _changes = [];
        public List<(string Step, int Count)> Steps { get; } = [];

        // Counts one change under a step name.
        private void Count(string step)
        {
            var i = Steps.FindIndex(s => s.Step == step);
            if (i < 0)
                Steps.Add((step, 1));
            else
                Steps[i] = (step, Steps[i].Count + 1);
        }

        // Hides the lines from one node to another, with a note line or none, if they hold nothing else and are not hidden yet; collapses run before line-level hiding so they win.
        public void Hide(SyntaxNode first, SyntaxNode last, string? note, string step)
        {
            var start = text.Lines.GetLineFromPosition(first.SpanStart).LineNumber;
            var end = text.Lines.GetLineFromPosition(last.Span.End).LineNumber;
            var leading = first.GetLeadingTrivia().Where(t => t.IsKind(SyntaxKind.SingleLineDocumentationCommentTrivia)).Select(t => text.Lines.GetLineFromPosition(t.SpanStart).LineNumber);
            if (note is not null && leading.Any())
                start = Math.Min(start, leading.Min());
            var firstLine = text.Lines.GetLineFromPosition(first.SpanStart);
            var before = text.ToString(TextSpan.FromBounds(firstLine.Start, first.SpanStart)).Trim();
            var after = text.ToString(TextSpan.FromBounds(last.Span.End, text.Lines[end].End)).Trim();
            if (before.Length > 0 || after.Length > 0 && !after.StartsWith("//"))
                return;
            if (_hidden.Any(h => h.Start <= end && start <= h.End))
                return;
            _hidden.Add((start, end, note));
            Count(step);
        }

        // Hides one line with no note.
        public void HideLine(int line, string step)
        {
            if (_hidden.Any(h => h.Start <= line && line <= h.End))
                return;
            _hidden.Add((line, line, null));
            Count(step);
        }

        // Records a change that stays within one line.
        public void Change(TextSpan span, string replacement, string step)
        {
            if (text.Lines.GetLineFromPosition(span.Start).LineNumber != text.Lines.GetLineFromPosition(span.End).LineNumber)
                return;
            _changes.Add(new TextChange(span, replacement));
            Count(step);
        }

        // Produces the view: hidden ranges become notes or gaps, changed lines are marked, and blank runs shrink to one.
        public List<ViewLine> Render()
        {
            var lines = new List<ViewLine>();
            for (var i = 0; i < text.Lines.Count; i++)
            {
                var hi = _hidden.FindIndex(h => h.Start <= i && i <= h.End);
                if (hi >= 0)
                {
                    var (start, end, note) = _hidden[hi];
                    if (note is not null)
                        lines.Add(new ViewLine(start + 1, end + 1, "⋯ " + note, false, true));
                    i = end;
                    continue;
                }
                var line = text.Lines[i];
                var mine = _changes.Where(c => line.Span.Contains(c.Span.Start)).OrderByDescending(c => c.Span.Start).ToList();
                var s = line.ToString();
                foreach (var c in mine)
                    s = s.Remove(c.Span.Start - line.Start, c.Span.Length).Insert(c.Span.Start - line.Start, c.NewText!);
                s = s.TrimEnd();
                if (s.Length == 0 && lines.Count > 0 && lines[^1].Text.Length == 0)
                    continue;
                lines.Add(new ViewLine(i + 1, i + 1, s, mine.Count > 0, false));
            }
            return lines;
        }
    }

    // Formats view lines for clients: `N\ttext`, `N~\ttext` for a changed line, `A-B\t⋯ note` for hidden lines.
    public static string Format(IEnumerable<ViewLine> lines) => string.Join('\n', lines.Select(l =>
        l.Note ? $"{l.Start}-{l.End}\t{l.Text}" : l.Changed ? $"{l.Start}~\t{l.Text}" : $"{l.Start}\t{l.Text}"));
}
