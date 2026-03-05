import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { BenefitCustomProperties, BenefitCustomProperties$Outbound } from "./benefitcustomproperties.js";
/**
 * A benefit of type `custom`.
 *
 * @remarks
 *
 * Use it to grant any kind of benefit that doesn't fit in the other types.
 */
export type BenefitCustom = {
    /**
     * Creation timestamp of the object.
     */
    createdAt: Date;
    /**
     * Last modification timestamp of the object.
     */
    modifiedAt: Date | null;
    /**
     * The ID of the benefit.
     */
    id: string;
    type?: "custom" | undefined;
    /**
     * The description of the benefit.
     */
    description: string;
    /**
     * Whether the benefit is selectable when creating a product.
     */
    selectable: boolean;
    /**
     * Whether the benefit is deletable.
     */
    deletable: boolean;
    /**
     * The ID of the organization owning the benefit.
     */
    organizationId: string;
    /**
     * Properties for a benefit of type `custom`.
     */
    properties: BenefitCustomProperties;
    /**
     * @deprecated field: This will be removed in a future release, please migrate away from it as soon as possible.
     */
    isTaxApplicable: boolean;
};
/** @internal */
export declare const BenefitCustom$inboundSchema: z.ZodType<BenefitCustom, z.ZodTypeDef, unknown>;
/** @internal */
export type BenefitCustom$Outbound = {
    created_at: string;
    modified_at: string | null;
    id: string;
    type: "custom";
    description: string;
    selectable: boolean;
    deletable: boolean;
    organization_id: string;
    properties: BenefitCustomProperties$Outbound;
    is_tax_applicable: boolean;
};
/** @internal */
export declare const BenefitCustom$outboundSchema: z.ZodType<BenefitCustom$Outbound, z.ZodTypeDef, BenefitCustom>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace BenefitCustom$ {
    /** @deprecated use `BenefitCustom$inboundSchema` instead. */
    const inboundSchema: z.ZodType<BenefitCustom, z.ZodTypeDef, unknown>;
    /** @deprecated use `BenefitCustom$outboundSchema` instead. */
    const outboundSchema: z.ZodType<BenefitCustom$Outbound, z.ZodTypeDef, BenefitCustom>;
    /** @deprecated use `BenefitCustom$Outbound` instead. */
    type Outbound = BenefitCustom$Outbound;
}
export declare function benefitCustomToJSON(benefitCustom: BenefitCustom): string;
export declare function benefitCustomFromJSON(jsonString: string): SafeParseResult<BenefitCustom, SDKValidationError>;
//# sourceMappingURL=benefitcustom.d.ts.map