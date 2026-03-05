import * as z from "zod";
import { ClosedEnum } from "../../types/enums.js";
export declare const CustomerBenefitGrantSortProperty: {
    readonly GrantedAt: "granted_at";
    readonly MinusGrantedAt: "-granted_at";
    readonly Type: "type";
    readonly MinusType: "-type";
    readonly Organization: "organization";
    readonly MinusOrganization: "-organization";
};
export type CustomerBenefitGrantSortProperty = ClosedEnum<typeof CustomerBenefitGrantSortProperty>;
/** @internal */
export declare const CustomerBenefitGrantSortProperty$inboundSchema: z.ZodNativeEnum<typeof CustomerBenefitGrantSortProperty>;
/** @internal */
export declare const CustomerBenefitGrantSortProperty$outboundSchema: z.ZodNativeEnum<typeof CustomerBenefitGrantSortProperty>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerBenefitGrantSortProperty$ {
    /** @deprecated use `CustomerBenefitGrantSortProperty$inboundSchema` instead. */
    const inboundSchema: z.ZodNativeEnum<{
        readonly GrantedAt: "granted_at";
        readonly MinusGrantedAt: "-granted_at";
        readonly Type: "type";
        readonly MinusType: "-type";
        readonly Organization: "organization";
        readonly MinusOrganization: "-organization";
    }>;
    /** @deprecated use `CustomerBenefitGrantSortProperty$outboundSchema` instead. */
    const outboundSchema: z.ZodNativeEnum<{
        readonly GrantedAt: "granted_at";
        readonly MinusGrantedAt: "-granted_at";
        readonly Type: "type";
        readonly MinusType: "-type";
        readonly Organization: "organization";
        readonly MinusOrganization: "-organization";
    }>;
}
//# sourceMappingURL=customerbenefitgrantsortproperty.d.ts.map