import * as z from "zod";
import { ClosedEnum } from "../../types/enums.js";
export declare const CustomerSortProperty: {
    readonly CreatedAt: "created_at";
    readonly MinusCreatedAt: "-created_at";
    readonly Email: "email";
    readonly MinusEmail: "-email";
    readonly Name: "name";
    readonly MinusName: "-name";
};
export type CustomerSortProperty = ClosedEnum<typeof CustomerSortProperty>;
/** @internal */
export declare const CustomerSortProperty$inboundSchema: z.ZodNativeEnum<typeof CustomerSortProperty>;
/** @internal */
export declare const CustomerSortProperty$outboundSchema: z.ZodNativeEnum<typeof CustomerSortProperty>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerSortProperty$ {
    /** @deprecated use `CustomerSortProperty$inboundSchema` instead. */
    const inboundSchema: z.ZodNativeEnum<{
        readonly CreatedAt: "created_at";
        readonly MinusCreatedAt: "-created_at";
        readonly Email: "email";
        readonly MinusEmail: "-email";
        readonly Name: "name";
        readonly MinusName: "-name";
    }>;
    /** @deprecated use `CustomerSortProperty$outboundSchema` instead. */
    const outboundSchema: z.ZodNativeEnum<{
        readonly CreatedAt: "created_at";
        readonly MinusCreatedAt: "-created_at";
        readonly Email: "email";
        readonly MinusEmail: "-email";
        readonly Name: "name";
        readonly MinusName: "-name";
    }>;
}
//# sourceMappingURL=customersortproperty.d.ts.map