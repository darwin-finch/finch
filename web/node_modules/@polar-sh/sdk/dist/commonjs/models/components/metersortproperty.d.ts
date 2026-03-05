import * as z from "zod";
import { ClosedEnum } from "../../types/enums.js";
export declare const MeterSortProperty: {
    readonly CreatedAt: "created_at";
    readonly MinusCreatedAt: "-created_at";
    readonly Name: "name";
    readonly MinusName: "-name";
};
export type MeterSortProperty = ClosedEnum<typeof MeterSortProperty>;
/** @internal */
export declare const MeterSortProperty$inboundSchema: z.ZodNativeEnum<typeof MeterSortProperty>;
/** @internal */
export declare const MeterSortProperty$outboundSchema: z.ZodNativeEnum<typeof MeterSortProperty>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace MeterSortProperty$ {
    /** @deprecated use `MeterSortProperty$inboundSchema` instead. */
    const inboundSchema: z.ZodNativeEnum<{
        readonly CreatedAt: "created_at";
        readonly MinusCreatedAt: "-created_at";
        readonly Name: "name";
        readonly MinusName: "-name";
    }>;
    /** @deprecated use `MeterSortProperty$outboundSchema` instead. */
    const outboundSchema: z.ZodNativeEnum<{
        readonly CreatedAt: "created_at";
        readonly MinusCreatedAt: "-created_at";
        readonly Name: "name";
        readonly MinusName: "-name";
    }>;
}
//# sourceMappingURL=metersortproperty.d.ts.map